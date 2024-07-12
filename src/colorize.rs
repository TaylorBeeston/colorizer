use crate::types::AppConfig;
use crate::utils::{
    apply_dithering, compute_integral_image, fast_spatial_color_average, lab_to_image_rgb,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, GenericImageView, ImageBuffer, Pixel, Rgb, RgbImage};
use indicatif::{ProgressBar, ProgressStyle};
use palette::color_difference::ImprovedCiede2000;
use palette::{IntoColor, Lab, Srgb};
use rayon::prelude::*;

pub fn colorize(img: &DynamicImage, config: &AppConfig) -> RgbImage {
    let (width, height) = img.dimensions();
    let total_pixels = (width * height) as u64;

    let first_pass_output = apply_color_mapping_and_dithering(img, config, total_pixels);
    apply_spatial_averaging_and_luminance_transfer(img, &first_pass_output, config, total_pixels)
}

fn apply_color_mapping_and_dithering(
    img: &DynamicImage,
    config: &AppConfig,
    total_pixels: u64,
) -> RgbImage {
    let (width, height) = img.dimensions();
    let progress_bar = create_progress_bar(
        total_pixels,
        "Applying Color Mapping and Dithering".to_string(),
    );
    let progress = Arc::new(AtomicU64::new(0));
    let output: Arc<Mutex<RgbImage>> = Arc::new(Mutex::new(ImageBuffer::new(width, height)));
    let color_map: Arc<Mutex<HashMap<[u8; 3], Lab>>> = Arc::new(Mutex::new(HashMap::new()));

    (0..total_pixels).into_par_iter().for_each(|i| {
        let (x, y) = get_coordinates(i, width);
        let pixel = img.get_pixel(x, y);
        let colorized_lab = memoized_find_closest_color(&color_map, pixel.to_rgb(), &config.colors);
        let dithered_color = apply_dithering(colorized_lab, colorized_lab, config.dither_amount);

        let new_pixel = lab_to_image_rgb(dithered_color);
        output.lock().unwrap().put_pixel(x, y, new_pixel);

        update_progress(&progress, &progress_bar);
    });

    progress_bar.finish_with_message("Color mapping and dithering complete");
    Arc::try_unwrap(output).unwrap().into_inner().unwrap()
}

fn apply_spatial_averaging_and_luminance_transfer(
    original_img: &DynamicImage,
    first_pass_output: &RgbImage,
    config: &AppConfig,
    total_pixels: u64,
) -> RgbImage {
    let (width, height) = original_img.dimensions();
    let progress_bar = create_progress_bar(
        total_pixels,
        "Applying Spatial Averaging and Luminance Transfer".to_string(),
    );
    let progress = Arc::new(AtomicU64::new(0));
    let final_output: Arc<Mutex<RgbImage>> = Arc::new(Mutex::new(ImageBuffer::new(width, height)));

    // Pre-compute integral image for faster spatial averaging
    let integral_image = compute_integral_image(first_pass_output);

    (0..total_pixels).into_par_iter().for_each(|i| {
        let (x, y) = get_coordinates(i, width);
        let original_lab = get_lab_color(original_img, x, y);
        let averaged_lab = fast_spatial_color_average(
            x,
            y,
            width,
            height,
            config.spatial_averaging_radius,
            &integral_image,
        );

        let final_lab = Lab::new(original_lab.l, averaged_lab.a, averaged_lab.b);
        let final_rgb = lab_to_image_rgb(final_lab);
        let blended_rgb = blend_colors(
            final_rgb,
            original_img.get_pixel(x, y).to_rgb(),
            config.blend_factor,
        );

        final_output.lock().unwrap().put_pixel(x, y, blended_rgb);

        update_progress(&progress, &progress_bar);
    });

    progress_bar.finish_with_message("Spatial averaging and luminance transfer complete");
    Arc::try_unwrap(final_output).unwrap().into_inner().unwrap()
}

fn create_progress_bar(total_pixels: u64, message: String) -> ProgressBar {
    let pb = ProgressBar::new(total_pixels);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {percent_precise}% ({eta})\n{msg}",
            )
            .unwrap()
            .progress_chars("#>-"),
    );
    pb.set_message(message);
    pb
}

fn get_coordinates(i: u64, width: u32) -> (u32, u32) {
    let x = (i % width as u64) as u32;
    let y = (i / width as u64) as u32;
    (x, y)
}

fn get_lab_color(img: &DynamicImage, x: u32, y: u32) -> Lab {
    let pixel = img.get_pixel(x, y);
    let rgb = Srgb::new(
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
    );
    rgb.into_color()
}

fn memoized_find_closest_color(
    color_map: &Arc<Mutex<HashMap<[u8; 3], Lab>>>,
    pixel: Rgb<u8>,
    colors: &[Lab],
) -> Lab {
    let key = [pixel[0], pixel[1], pixel[2]];

    if let Some(&lab) = color_map.lock().unwrap().get(&key) {
        return lab;
    }

    let original_rgb = Srgb::new(
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
    );
    let original_lab: Lab = original_rgb.into_color();
    let closest_color = find_closest_color(&original_lab, colors);
    let colorized_lab = Lab::new(original_lab.l, closest_color.a, closest_color.b);

    color_map.lock().unwrap().insert(key, colorized_lab);

    colorized_lab
}

fn find_closest_color<'a>(original: &Lab, colors: &'a [Lab]) -> &'a Lab {
    colors
        .iter()
        .min_by(|&&a, &&b| {
            original
                .improved_difference(a)
                .partial_cmp(&original.improved_difference(b))
                .unwrap()
        })
        .unwrap()
}

fn blend_colors(color1: Rgb<u8>, color2: Rgb<u8>, blend_factor: f32) -> Rgb<u8> {
    Rgb([
        ((color1[0] as f32 * blend_factor + color2[0] as f32 * (1.0 - blend_factor)) as u8)
            .clamp(0, 255),
        ((color1[1] as f32 * blend_factor + color2[1] as f32 * (1.0 - blend_factor)) as u8)
            .clamp(0, 255),
        ((color1[2] as f32 * blend_factor + color2[2] as f32 * (1.0 - blend_factor)) as u8)
            .clamp(0, 255),
    ])
}

fn update_progress(progress: &AtomicU64, progress_bar: &ProgressBar) {
    let prev_count = progress.fetch_add(1, Ordering::Relaxed);
    if prev_count % 5000 == 0 {
        progress_bar.set_position(prev_count);
    }
}
