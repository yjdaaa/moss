use std::path::Path;

use image::{GrayImage, ImageReader, Luma, imageops::FilterType};

pub fn decode_grayscale(path: impl AsRef<Path>) -> image::ImageResult<GrayImage> {
    Ok(ImageReader::open(path)?.decode()?.to_luma8())
}

pub fn invert(image: &mut GrayImage) {
    for pixel in image.pixels_mut() {
        pixel.0[0] = 255 - pixel.0[0];
    }
}

pub fn resize(image: &GrayImage, width: u32, height: u32) -> GrayImage {
    image::imageops::resize(image, width, height, FilterType::Triangle)
}

pub fn resize_area(image: &GrayImage, width: u32, height: u32) -> GrayImage {
    if width == 0 || height == 0 || image.width() == 0 || image.height() == 0 {
        return GrayImage::new(width, height);
    }
    GrayImage::from_fn(width, height, |x, y| {
        let x0 = x as f64 * image.width() as f64 / width as f64;
        let x1 = (x + 1) as f64 * image.width() as f64 / width as f64;
        let y0 = y as f64 * image.height() as f64 / height as f64;
        let y1 = (y + 1) as f64 * image.height() as f64 / height as f64;
        let ix0 = x0.floor() as u32;
        let ix1 = (x1.ceil() as u32).min(image.width());
        let iy0 = y0.floor() as u32;
        let iy1 = (y1.ceil() as u32).min(image.height());
        let mut total = 0.0;
        let mut weight = 0.0;
        for sy in iy0..iy1 {
            let wy = (y1.min((sy + 1) as f64) - y0.max(sy as f64)).max(0.0);
            for sx in ix0..ix1 {
                let wx = (x1.min((sx + 1) as f64) - x0.max(sx as f64)).max(0.0);
                let area = wx * wy;
                total += f64::from(image.get_pixel(sx, sy).0[0]) * area;
                weight += area;
            }
        }
        Luma([((total / weight.max(f64::EPSILON)).round() as u32).min(255) as u8])
    })
}

pub fn resize_nearest(image: &GrayImage, grid_width: u32, grid_height: u32) -> GrayImage {
    if grid_width == 0 || grid_height == 0 || image.width() == 0 || image.height() == 0 {
        return GrayImage::new(grid_width, grid_height);
    }
    let (width, height) = (image.width(), image.height());
    GrayImage::from_fn(grid_width, grid_height, |x_index, y_index| {
        let source_y =
            ((y_index as u64 * height as u64) / grid_height as u64).min((height - 1) as u64) as u32;
        let source_x =
            ((x_index as u64 * width as u64) / grid_width as u64).min((width - 1) as u64) as u32;
        *image.get_pixel(source_x, source_y)
    })
}

pub fn resize_darkest(image: &GrayImage, grid_width: u32, grid_height: u32) -> GrayImage {
    if grid_width == 0 || grid_height == 0 || image.width() == 0 || image.height() == 0 {
        return GrayImage::new(grid_width, grid_height);
    }
    let (width, height) = (image.width(), image.height());
    GrayImage::from_fn(grid_width, grid_height, |x_index, y_index| {
        let y0 = ((y_index as u64 * height as u64) / grid_height as u64) as u32;
        let y1 = (((y_index + 1) as u64 * height as u64) / grid_height as u64).max((y0 + 1) as u64)
            as u32;
        let x0 = ((x_index as u64 * width as u64) / grid_width as u64) as u32;
        let x1 =
            (((x_index + 1) as u64 * width as u64) / grid_width as u64).max((x0 + 1) as u64) as u32;
        let mut darkest = 255_u8;
        for sy in y0..y1.min(height) {
            for sx in x0..x1.min(width) {
                let val = image.get_pixel(sx, sy).0[0];
                if val < darkest {
                    darkest = val;
                }
            }
        }
        Luma([darkest])
    })
}

pub fn adjust_brightness(image: &mut GrayImage, factor: f32) {
    if (factor - 1.0).abs() < 1e-4 {
        return;
    }
    for pixel in image.pixels_mut() {
        let val = (pixel.0[0] as f32 * factor).round().clamp(0.0, 255.0) as u8;
        pixel.0[0] = val;
    }
}

pub fn adjust_contrast(image: &mut GrayImage, factor: f32) {
    if (factor - 1.0).abs() < 1e-4 {
        return;
    }
    let sum: u64 = image.pixels().map(|p| p.0[0] as u64).sum();
    let count = (image.width() * image.height()) as f32;
    let mean = if count > 0.0 {
        sum as f32 / count
    } else {
        128.0
    };
    for pixel in image.pixels_mut() {
        let val = (mean + (pixel.0[0] as f32 - mean) * factor)
            .round()
            .clamp(0.0, 255.0) as u8;
        pixel.0[0] = val;
    }
}

pub fn crop_checked(
    image: &GrayImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<GrayImage, &'static str> {
    if width == 0
        || height == 0
        || x > image.width()
        || y > image.height()
        || width > image.width() - x
        || height > image.height() - y
    {
        return Err("crop rectangle is outside the image");
    }
    Ok(crop(image, x, y, width, height))
}

pub fn crop(image: &GrayImage, x: u32, y: u32, width: u32, height: u32) -> GrayImage {
    image::imageops::crop_imm(image, x, y, width, height).to_image()
}

pub fn otsu_threshold(image: &GrayImage) -> u8 {
    let mut histogram = [0_u64; 256];
    for pixel in image.pixels() {
        histogram[pixel.0[0] as usize] += 1;
    }
    let total = image.width() as f64 * image.height() as f64;
    let total_sum: f64 = histogram
        .iter()
        .enumerate()
        .map(|(value, count)| value as f64 * *count as f64)
        .sum();
    let mut background_count = 0.0;
    let mut background_sum = 0.0;
    let mut best = (-1.0, 128_u8);
    for threshold in 0..=255 {
        background_count += histogram[threshold] as f64;
        if background_count == 0.0 || background_count == total {
            continue;
        }
        background_sum += threshold as f64 * histogram[threshold] as f64;
        let foreground_count = total - background_count;
        let mean_background = background_sum / background_count;
        let mean_foreground = (total_sum - background_sum) / foreground_count;
        let between =
            background_count * foreground_count * (mean_background - mean_foreground).powi(2);
        if between > best.0 {
            best = (between, threshold as u8);
        }
    }
    best.1
}

pub fn threshold(image: &GrayImage, value: u8) -> GrayImage {
    GrayImage::from_fn(image.width(), image.height(), |x, y| {
        Luma([if image.get_pixel(x, y).0[0] > value {
            255
        } else {
            0
        }])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_are_deterministic() {
        let image = GrayImage::from_fn(4, 2, |x, _| Luma([(x * 80) as u8]));
        assert_eq!(otsu_threshold(&image), otsu_threshold(&image));
        assert_eq!(resize_area(&image, 2, 1).dimensions(), (2, 1));
        assert_eq!(resize_nearest(&image, 2, 1).dimensions(), (2, 1));
        assert_eq!(resize_darkest(&image, 2, 1).dimensions(), (2, 1));
        assert_eq!(crop(&image, 1, 0, 2, 2).dimensions(), (2, 2));
        assert!(crop_checked(&image, 3, 0, 2, 1).is_err());
        let mut inverted = image.clone();
        invert(&mut inverted);
        assert_eq!(inverted.get_pixel(0, 0).0[0], 255);

        let mut bright = GrayImage::from_pixel(2, 2, Luma([100]));
        adjust_brightness(&mut bright, 1.5);
        assert_eq!(bright.get_pixel(0, 0).0[0], 150);

        let mut contrast = GrayImage::from_pixel(2, 2, Luma([100]));
        adjust_contrast(&mut contrast, 1.2);
        assert_eq!(contrast.get_pixel(0, 0).0[0], 100);
    }

    #[test]
    fn uniform_otsu_uses_python_default() {
        let image = GrayImage::from_pixel(2, 2, Luma([80]));
        assert_eq!(otsu_threshold(&image), 128);
    }
}
