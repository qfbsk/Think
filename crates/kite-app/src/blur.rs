//! A blur the theme can dial: three box passes, the cheap stand-in for a
//! Gaussian.

use image::RgbaImage;

/// Blurs in place, `radius` pixels of source image per side. Edges clamp.
pub fn box_blur(img: &mut RgbaImage, radius: u32) {
    if radius == 0 {
        return;
    }
    let (w, h) = img.dimensions();
    let (w, h) = (w as usize, h as usize);
    let r = radius as usize;
    let win = (2 * r + 1) as f32;
    let mut src: Vec<f32> = img
        .pixels()
        .flat_map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
        .collect();
    let mut dst = vec![0.0f32; src.len()];

    for _ in 0..3 {
        for y in 0..h {
            let row = y * w * 3;
            let mut sum = [0.0f32; 3];
            for x in 0..w {
                if x == 0 {
                    for i in 0..=2 * r {
                        let at = (i as isize - r as isize).clamp(0, w as isize - 1) as usize;
                        add(&src, row + at * 3, &mut sum);
                    }
                } else {
                    add(&src, row + (x + r).min(w - 1) * 3, &mut sum);
                    sub(&src, row + (x as isize - r as isize - 1).max(0) as usize * 3, &mut sum);
                }
                write(&mut dst, row + x * 3, &sum, win);
            }
        }
        std::mem::swap(&mut src, &mut dst);
        for x in 0..w {
            let col = x * 3;
            let mut sum = [0.0f32; 3];
            for y in 0..h {
                if y == 0 {
                    for i in 0..=2 * r {
                        let at = (i as isize - r as isize).clamp(0, h as isize - 1) as usize;
                        add(&src, at * w * 3 + col, &mut sum);
                    }
                } else {
                    add(&src, (y + r).min(h - 1) * w * 3 + col, &mut sum);
                    sub(&src, (y as isize - r as isize - 1).max(0) as usize * w * 3 + col, &mut sum);
                }
                write(&mut dst, y * w * 3 + col, &sum, win);
            }
        }
        std::mem::swap(&mut src, &mut dst);
    }

    for (p, chunk) in img.pixels_mut().zip(src.chunks(3)) {
        for c in 0..3 {
            p.0[c] = chunk[c].round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn add(src: &[f32], at: usize, sum: &mut [f32; 3]) {
    for c in 0..3 {
        sum[c] += src[at + c];
    }
}

fn sub(src: &[f32], at: usize, sum: &mut [f32; 3]) {
    for c in 0..3 {
        sum[c] -= src[at + c];
    }
}

fn write(dst: &mut [f32], at: usize, sum: &[f32; 3], win: f32) {
    for c in 0..3 {
        dst[at + c] = sum[c] / win;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba([v, v, v, 255]))
    }

    #[test]
    fn a_flat_picture_stays_flat() {
        let mut img = solid(24, 16, 128);
        box_blur(&mut img, 5);
        assert!(img.pixels().all(|p| p.0[0] == 128 && p.0[2] == 128));
    }

    #[test]
    fn zero_radius_is_a_no_op() {
        let mut img = RgbaImage::from_pixel(8, 8, image::Rgba([10, 200, 77, 255]));
        box_blur(&mut img, 0);
        assert!(img.pixels().all(|p| p.0 == [10, 200, 77, 255]));
    }

    #[test]
    fn one_dot_spreads_to_the_trinomial_weights() {
        // Three box passes of radius 1 convolve to [1,3,6,7,6,3,1]/27 per axis,
        // so a lone lit pixel keeps (7/27)^2 of itself at the centre.
        let mut img = solid(31, 31, 0);
        img.put_pixel(15, 15, image::Rgba([255, 255, 255, 255]));
        box_blur(&mut img, 1);
        let want = (255.0f64 * (7.0f64 / 27.0).powi(2)).round() as u8;
        assert_eq!(img.get_pixel(15, 15)[0], want, "centre keeps (7/27)^2 of the dot");
        let arm = (255.0f64 * (6.0f64 / 27.0) * (7.0 / 27.0)).round() as u8;
        assert_eq!(img.get_pixel(14, 15)[0], arm);
    }
}
