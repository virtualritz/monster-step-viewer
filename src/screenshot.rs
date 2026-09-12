//! Native viewport PNG screenshots and clipboard export.

use arboard::{Clipboard, ImageData};
use bevy::{
    camera::Viewport,
    color::{Color, ColorToPacked},
    ecs::observer::On,
    prelude::{Camera, Commands, Component, Query, With},
    render::{
        render_resource::TextureFormat,
        view::screenshot::{Screenshot, ScreenshotCaptured},
    },
};
use png::{BitDepth, ColorType, Encoder};
use rayon::prelude::*;
use std::{
    borrow::Cow,
    fs::{File, create_dir_all},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::state::MainCamera;

#[derive(Component)]
pub(crate) struct ScreenshotSaveRequest {
    viewport: Viewport,
    path: PathBuf,
    background: [u8; 3],
}

/// Queue a viewport-only screenshot. The asynchronous readback is cropped and
/// encoded after the frame has been rendered.
pub(crate) fn request_viewport_screenshot(
    commands: &mut Commands,
    cameras: &mut Query<&mut Camera, With<MainCamera>>,
    clear_color: Color,
) -> Result<(), String> {
    let camera = cameras.single_mut().map_err(|_| {
        "Cannot save screenshot: main camera is unavailable".to_string()
    })?;
    let viewport = camera.viewport.clone().ok_or_else(|| {
        "Cannot save screenshot: viewport has not been sized yet".to_string()
    })?;
    let path = screenshot_path()?;
    let background = clear_color.to_srgba().to_u8_array_no_alpha();

    commands.spawn((
        Screenshot::primary_window(),
        ScreenshotSaveRequest {
            viewport,
            path,
            background,
        },
    ));
    Ok(())
}

pub(crate) fn save_captured_viewport(
    captured: On<ScreenshotCaptured>,
    requests: Query<&ScreenshotSaveRequest>,
) {
    let Ok(request) = requests.get(captured.entity) else {
        return;
    };
    match write_captured_viewport(&captured.image, request) {
        Ok(()) => log::info!(
            "Viewport screenshot saved to {} and copied to the clipboard",
            request.path.display()
        ),
        Err(error) => {
            log::error!("Could not save viewport screenshot: {error}")
        }
    }
}

fn screenshot_path() -> Result<PathBuf, String> {
    let directory = platform_screenshot_directory()?;
    create_dir_all(&directory).map_err(|error| {
        format!(
            "Could not create screenshot directory {}: {error}",
            directory.display()
        )
    })?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("Could not name screenshot: {error}"))?
        .as_millis();
    Ok(directory.join(format!("Monster STEP Viewer {millis}.png")))
}

fn platform_screenshot_directory() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    let directory = dirs::desktop_dir();
    #[cfg(target_os = "windows")]
    let directory = dirs::picture_dir().map(|path| path.join("Screenshots"));
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let directory = dirs::picture_dir().map(|path| path.join("Screenshots"));

    directory.ok_or_else(|| {
        "Could not determine the platform screenshot directory".to_string()
    })
}

fn write_captured_viewport(
    image: &bevy::prelude::Image,
    request: &ScreenshotSaveRequest,
) -> Result<(), String> {
    let viewport = &request.viewport;
    let image_width = image.width() as usize;
    let image_height = image.height() as usize;
    let rgba = image
        .data
        .as_ref()
        .ok_or_else(|| "Screenshot readback contained no pixels".to_string())?;
    let mut pixels = crop_rgba8(
        rgba,
        image_width,
        image_height,
        viewport.physical_position.x as usize,
        viewport.physical_position.y as usize,
        viewport.physical_size.x as usize,
        viewport.physical_size.y as usize,
    )?;
    match image.texture_descriptor.format {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => {}
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb => {
            pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .for_each(|pixel| pixel.swap(0, 2));
        }
        format => {
            return Err(format!(
                "Screenshot uses unsupported pixel format {format:?}"
            ));
        }
    }
    make_background_transparent(&mut pixels, request.background);

    let file = File::create(&request.path).map_err(|error| {
        format!("Could not create {}: {error}", request.path.display())
    })?;
    let mut encoder =
        Encoder::new(file, viewport.physical_size.x, viewport.physical_size.y);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("Could not write PNG header: {error}"))?;
    writer
        .write_image_data(&pixels)
        .map_err(|error| format!("Could not encode PNG: {error}"))?;

    let mut clipboard = Clipboard::new()
        .map_err(|error| format!("Could not access clipboard: {error}"))?;
    clipboard
        .set_image(ImageData {
            width: viewport.physical_size.x as usize,
            height: viewport.physical_size.y as usize,
            bytes: Cow::Owned(pixels),
        })
        .map_err(|error| {
            format!("Could not copy screenshot to clipboard: {error}")
        })
}

fn crop_rgba8(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, String> {
    let source_len = source_width
        .checked_mul(source_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "Screenshot dimensions overflow".to_string())?;
    if source.len() != source_len
        || x + width > source_width
        || y + height > source_height
    {
        return Err(
            "Screenshot dimensions do not match the viewport".to_string()
        );
    }
    let row_len = width * 4;
    Ok(source
        .par_chunks_exact(source_width * 4)
        .skip(y)
        .take(height)
        .flat_map_iter(|row| {
            let start = x * 4;
            row[start..start + row_len].iter().copied()
        })
        .collect())
}

fn make_background_transparent(pixels: &mut [u8], background: [u8; 3]) {
    pixels.par_chunks_exact_mut(4).for_each(|pixel| {
        if pixel[..3]
            .iter()
            .zip(background)
            .all(|(channel, expected)| channel.abs_diff(expected) <= 1)
        {
            pixel[3] = 0;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::crop_rgba8;

    #[test]
    fn crops_viewport_rows_without_ui_pixels() {
        let source: Vec<u8> = (0..48).collect();
        let cropped = crop_rgba8(&source, 4, 3, 1, 1, 2, 2).unwrap();
        assert_eq!(cropped, (20..28).chain(36..44).collect::<Vec<_>>());
    }

    #[test]
    fn makes_clear_color_transparent() {
        let mut pixels = vec![43, 44, 47, 255, 44, 44, 47, 255];
        super::make_background_transparent(&mut pixels, [43, 44, 47]);
        assert_eq!(pixels, [43, 44, 47, 0, 44, 44, 47, 0]);
    }
}
