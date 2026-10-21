/// Fractional-delay line with cubic (Hermite) interpolation.
pub struct DelayLine {
    buf: Vec<f32>,
    w: usize,
    mask: usize,
}

impl DelayLine {
    pub fn new(min_length: usize) -> Self {
        let mut n = 1;
        while n < min_length + 4 {
            n <<= 1;
        }
        DelayLine { buf: vec![0.0; n], w: 0, mask: n - 1 }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len() - 4
    }

    #[inline]
    pub fn write(&mut self, x: f64) {
        self.w = (self.w + 1) & self.mask;
        self.buf[self.w] = x as f32;
    }

    #[inline]
    pub fn read(&self, delay: f64) -> f64 {
        let delay = delay.clamp(1.0, self.capacity() as f64);
        let rp = self.w as f64 - delay;
        let i = rp.floor();
        let f = rp - i;
        let i = i as isize;
        let m = self.mask as isize;
        let at = |k: isize| self.buf[(k & m) as usize] as f64;
        let (xm1, x0, x1, x2) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * f + c2) * f + c1) * f + x0
    }
}
