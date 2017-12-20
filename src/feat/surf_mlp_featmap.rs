use common::Rectangle;
use feat::FeatureMap;
use math;

pub struct SurfMlpFeatureMap {
    roi: Option<Rectangle>,
    width: u32,
    height: u32,
    length: usize,
    buf_valid_reset: bool,
    feature_pool: FeaturePool,
    feature_vectors: Vec<Vec<i32>>,
    feature_vectors_normalized: Vec<Vec<f32>>,
    feature_valid_indicators: Vec<bool>,
    grad_x: Vec<i32>,
    grad_y: Vec<i32>,
    int_img: Vec<i32>,
    img_buf: Vec<i32>,
}

impl FeatureMap for SurfMlpFeatureMap {
    fn compute(&mut self, input: *const u8, width: u32, height: u32) {
        if width == 0 || height == 0 {
            panic!(format!("Illegal arguments: width ({}), height ({})", width, height));
        }

        self.reshape(width, height);
        self.compute_gradient_images(input);
        self.compute_integral_images();
    }
}

impl SurfMlpFeatureMap {
    pub fn new() -> Self {
        let feature_pool = SurfMlpFeatureMap::create_feature_pool();
        let feature_pool_size = feature_pool.size();
        let mut feature_vectors = Vec::with_capacity(feature_pool_size);
        let mut feature_vectors_normalized = Vec::with_capacity(feature_pool_size);
        for feature_id in 0..feature_pool_size {
            let dim = feature_pool.get_feature_vector_dim(feature_id);
            feature_vectors[feature_id] = Vec::with_capacity(dim);
            feature_vectors_normalized[feature_id] = Vec::with_capacity(dim);
        }
        let feature_valid_indicators = Vec::with_capacity(feature_pool_size);

        SurfMlpFeatureMap {
            roi: None,
            width: 0,
            height: 0,
            length: 0,
            buf_valid_reset: false,
            feature_pool,
            feature_vectors,
            feature_vectors_normalized,
            feature_valid_indicators,
            grad_x: vec![],
            grad_y: vec![],
            int_img: vec![],
            img_buf: vec![],
        }
    }

    fn create_feature_pool() -> FeaturePool {
        let mut feature_pool = FeaturePool::new();
        feature_pool.add_patch_format(1, 1, 2, 2);
        feature_pool.add_patch_format(1, 2, 2, 2);
        feature_pool.add_patch_format(2, 1, 2, 2);
        feature_pool.add_patch_format(2, 3, 2, 2);
        feature_pool.add_patch_format(3, 2, 2, 2);
        feature_pool.create();
        feature_pool
    }

    fn reshape(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.length = (width * height) as usize;

        self.grad_x.resize(self.length, 0);
        self.grad_y.resize(self.length, 0);
        self.int_img.resize(self.length * FeaturePool::K_NUM_INT_CHANNEL as usize, 0);
        self.img_buf.resize(self.length, 0);
    }

    fn compute_gradient_images(&mut self, input: *const u8) {
        unsafe {
            math::copy_u8_to_i32(input, self.int_img.as_mut_ptr(), self.length);
        }
        self.compute_grad_x();
        self.compute_grad_y();
    }

    fn compute_grad_x(&mut self) {
        let input = self.int_img.as_ptr();
        let dx = self.grad_x.as_mut_ptr();
        let len = (self.width - 2) as usize;

        unsafe {
            for r in 0..self.height {
                let offset = (r * self.width) as isize;
                let mut src = input.offset(offset);
                let mut dest = dx.offset(offset);
                *dest = ((*(src.offset(1))) - (*src)) << 1;
                math::vector_sub(src.offset(2), src, dest.offset(1), len);

                let offset = (self.width - 1) as isize;
                src = src.offset(offset);
                dest = dest.offset(offset);
                *dest = ((*src) - (*(src.offset(-1)))) << 1;
            }
        }
    }

    fn compute_grad_y(&mut self) {
        let input = self.int_img.as_ptr();
        let mut dy = self.grad_y.as_mut_ptr();
        let len = self.width as usize;

        unsafe {
            math::vector_sub(input.offset(self.width as isize), input, dy, len);
            math::vector_add(dy, dy, dy, len);

            for r in 1..(self.height - 1) {
                let src = input.offset(((r - 1) * self.width) as isize);
                let dest = dy.offset((r * self.width) as isize);
                math::vector_sub(src.offset((self.width << 1) as isize), src, dest, len);
            }

            let offset = ((self.height - 1) * self.width) as isize;
            dy = dy.offset(offset);
            math::vector_sub(input.offset(offset), input.offset(offset - self.width as isize), dy, len);
            math::vector_add(dy, dy, dy, len);
        }
    }

    fn compute_integral_images(&mut self) {
        let grad_x_ptr = self.grad_x.as_ptr();
        let grad_y_ptr = self.grad_y.as_ptr();
        let img_buf_ptr = self.img_buf.as_ptr();

        unsafe {
            self.fill_integral_channel(grad_x_ptr, 0);
            self.fill_integral_channel(grad_y_ptr, 4);
            math::abs(grad_x_ptr, img_buf_ptr as *mut i32, self.length);
            self.fill_integral_channel(img_buf_ptr, 1);
            math::abs(grad_y_ptr, img_buf_ptr as *mut i32, self.length);
            self.fill_integral_channel(img_buf_ptr, 5);
        }

        self.mask_integral_channel();
        self.integral();
    }

    unsafe fn fill_integral_channel(&mut self, mut src: *const i32, ch: u32) {
        let mut dest = self.int_img.as_mut_ptr().offset(ch as isize);
        for _ in 0..self.length {
            *dest = *src;
            *dest.offset(2) = *src;
            dest = dest.offset(FeaturePool::K_NUM_INT_CHANNEL as isize);
            src = src.offset(1);
        }
    }

    fn mask_integral_channel(&mut self) {
        let grad_x_ptr = self.grad_x.as_ptr();
        let grad_y_ptr = self.grad_y.as_ptr();

        let mut dx: i32;
        let mut dy: i32;
        let mut dx_mask: i32;
        let mut dy_mask: i32;
        let mut cmp: u32;
        let xor_bits: Vec<u32> = vec![0xffffffff, 0xffffffff, 0, 0];

        let mut src = self.int_img.as_mut_ptr();
        unsafe {
            for i in 0..self.length {
                dx = *grad_x_ptr.offset(1);
                dy = *grad_y_ptr.offset(1);

                cmp = if dy < 0 { 0xffffffff } else { 0x0 };
                for j in 0..4 {
                    dy_mask = (cmp ^ xor_bits[j]) as i32;
                    *src = *src & dy_mask;
                    src = src.offset(1);
                }

                cmp = if dx < 0 { 0xffffffff } else { 0x0 };
                for j in 0..4 {
                    dx_mask = (cmp ^ xor_bits[j]) as i32;
                    *src = *src & dx_mask;
                    src = src.offset(1);
                }
            }
        }
    }

    fn integral(&mut self) {
        let data = self.int_img.as_ptr();
        let len = (FeaturePool::K_NUM_INT_CHANNEL * self.width) as usize;

        unsafe {
            for r in 0..(self.height - 1) as isize {
                let row1 = data.offset(r * len as isize);
                let row2 = row1.offset(len as isize);
                math::vector_add(row1, row2, row2 as *mut i32, len);
            }

            for r in 0..self.height as isize {
                SurfMlpFeatureMap::vector_cumulative_add(
                    data.offset(r * len as isize), len, FeaturePool::K_NUM_INT_CHANNEL);
            }
        }
    }

    unsafe fn vector_cumulative_add(x: *const i32, len: usize, num_channel: u32) {
        let num_channel = num_channel as usize;
        let cols = len / num_channel - 1;
        for i in 0..cols as isize {
            let col1 = x.offset(i * num_channel as isize);
            let col2 = col1.offset(num_channel as isize);
            math::vector_add(col1, col2, col2 as *mut i32, num_channel);
        }
    }
}

struct FeaturePool {
    sample_width: u32,
    sample_height: u32,
    patch_move_step_x: u32,
    patch_move_step_y: u32,
    patch_size_inc_step: u32,
    patch_min_width: u32,
    patch_min_height: u32,
    features: Vec<Feature>,
    patch_formats: Vec<PatchFormat>,
}

impl FeaturePool {
    const K_NUM_INT_CHANNEL: u32 = 8;

    fn new() -> Self {
        FeaturePool {
            sample_width: 0,
            sample_height: 0,
            patch_move_step_x: 0,
            patch_move_step_y: 0,
            patch_size_inc_step: 0,
            patch_min_width: 0,
            patch_min_height: 0,
            features: vec![],
            patch_formats: vec![],
        }
    }

    fn add_patch_format(&mut self, width: u32, height: u32, num_cell_per_row: u32, num_cell_per_col: u32) {
        self.patch_formats.push(
            PatchFormat { width, height, num_cell_per_row, num_cell_per_col }
        );
    }

    fn create(&mut self) {
        let mut feature_vecs = vec![];

        if self.sample_height - self.patch_min_height <= self.sample_width - self.patch_min_width {
            for ref format in self.patch_formats.iter() {
                let mut h = self.patch_min_height;
                while h <= self.sample_height {
                    if h % format.num_cell_per_col != 0 || h % format.height != 0 {
                        continue;
                    }
                    let w = h / format.height * format.width;
                    if w % format.num_cell_per_row != 0 || w < self.patch_min_width || w > self.sample_width {
                        continue;
                    }
                    self.collect_features(w, h, format.num_cell_per_row, format.num_cell_per_col, &mut feature_vecs);
                    h += self.patch_size_inc_step;
                }
            }
        } else {
            for ref format in self.patch_formats.iter() {
                let mut w = self.patch_min_width;
                // original condition was <= self.patch_min_width,
                // but it would not make sense to have a loop in such case
                while w <= self.sample_width {
                    if w % format.num_cell_per_row != 0 || w % format.width != 0 {
                        continue;
                    }
                    let h = w / format.width * format.height;
                    if h % format.num_cell_per_col != 0 || h < self.patch_min_height || h > self.sample_height {
                        continue;
                    }
                    self.collect_features(w, h, format.num_cell_per_row, format.num_cell_per_col, &mut feature_vecs);
                    w += self.patch_size_inc_step;
                }
            }
        }

        self.features.append(&mut feature_vecs);
    }

    fn collect_features(&self, width: u32, height: u32, num_cell_per_row: u32, num_cell_per_col: u32, dest: &mut Vec<Feature>) {
        let y_lim = self.sample_height - height;
        let x_lim = self.sample_width - width;

        let mut y = 0;
        while y <= y_lim {
            let mut x = 0;
            while x <= x_lim {
                dest.push(
                    Feature {
                        patch: Rectangle::new(x, y, width, height),
                        num_cell_per_row, num_cell_per_col
                    }
                );
                x += self.patch_move_step_x;
            }
            y += self.patch_move_step_y;
        }
    }

    fn size(&self) -> usize {
        self.features.len()
    }

    fn get_feature_vector_dim(&self, feature_id: usize) -> usize {
        let feature = &self.features[feature_id];
        (feature.num_cell_per_col * feature.num_cell_per_row * FeaturePool::K_NUM_INT_CHANNEL) as usize
    }
}

struct PatchFormat {
    width: u32,
    height: u32,
    num_cell_per_row: u32,
    num_cell_per_col: u32,
}

struct Feature {
    patch: Rectangle,
    num_cell_per_row: u32,
    num_cell_per_col: u32,
}