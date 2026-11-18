//! The 256 x 240 picture of the core, uploaded as an RGBA texture for the 2D mode of
//! SPEC-06 REQ-SCN-055 and AMD-009 (Q-022). XSnes3D+: the core hands out red, green, blue and
//! alpha for each pixel, so the bytes go to the texture as they are. The texture is sampled
//! nearest without mipmap like every other texture of the game (REQ-E3D-563).

use glow::Texture;

use super::api::{Gl, GlError};
use super::texture_cache::set_nearest_clamp;

/// Width of the pixel frame.
pub const WIDTH: usize = 256;

/// Height of the pixel frame.
pub const HEIGHT: usize = 240;

/// The picture texture, created on first upload; released when dropped.
#[derive(Debug)]
pub struct NesPictureTexture {
    gl: Gl,
    pixels: Vec<u8>,
    handle: Option<Texture>,
    /// Size of the texture allocated: 256 x 240, or larger for a widescreen picture.
    size: (usize, usize),
}

impl NesPictureTexture {
    /// Creates the texture for the current context.
    pub fn new(gl: &Gl) -> Self {
        Self {
            gl: gl.clone(),
            pixels: vec![0; WIDTH * HEIGHT * 4],
            handle: None,
            size: (WIDTH, HEIGHT),
        }
    }

    /// Uploads `indices` (red, green, blue and alpha, row 0 first, `height` rows of 256 or of a
    /// wider width) and returns the texture, or `None` when the frame has no such size.
    pub fn upload(&mut self, indices: &[u8], height: usize) -> Result<Option<Texture>, GlError> {
        let Some(size) = crate::interop::picture_size(indices.len(), height) else {
            return Ok(None);
        };
        if size != self.size {
            // Another size: a new texture of the right size.
            if let Some(handle) = self.handle.take() {
                self.gl.delete_texture(handle);
            }
            self.size = size;
        }
        self.pixels.resize(size.0 * size.1 * 4, 0);

        // AMD-050 REQ-SCN-076: a pixel whose opacity bit is 0 — the universal backdrop for want
        // of an opaque pixel (REQ-CORE-049) — gets alpha 0, which `scene.frag` already rejects
        // in the opaque pass, with no sorting, no separate depth pass and no alpha blending.
        // An **opaque** pixel whose palette colour happens to be the universal backdrop's keeps
        // alpha 255 and stays drawn, which is the whole point of the mask: the index image
        // cannot tell the two apart.
        //
        // This loop already ran once per frame in 2D mode and already wrote alpha 0 for a pixel
        // the overscan leaves out, so the mask costs it one `AND` and one select per pixel, in
        // 2D mode only.
        // XSnes3D+: the picture is already red, green, blue and alpha (alpha 0: not drawn,
        // which `scene.frag` rejects in the opaque pass).
        let length = self.pixels.len();
        self.pixels.copy_from_slice(&indices[..length]);

        let gl = &self.gl;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)] // At most 1280.
        let (width, height) = (size.0 as i32, size.1 as i32);
        let existing = self.handle;
        let handle = match existing {
            Some(handle) => handle,
            None => gl.create_texture()?,
        };
        gl.bind_texture(glow::TEXTURE_2D, Some(handle));
        gl.pixel_store(glow::UNPACK_ALIGNMENT, 1);
        if existing.is_some() {
            gl.tex_sub_image_2d(glow::TEXTURE_2D, width, height, glow::RGBA, &self.pixels);
        } else {
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                glow::RGBA8,
                width,
                height,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&self.pixels),
            );
            set_nearest_clamp(gl);
            self.handle = Some(handle);
        }

        gl.pixel_store(glow::UNPACK_ALIGNMENT, 4);
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(Some(handle))
    }
}

impl Drop for NesPictureTexture {
    fn drop(&mut self) {
        if let Some(handle) = self.handle {
            self.gl.delete_texture(handle);
        }
    }
}
