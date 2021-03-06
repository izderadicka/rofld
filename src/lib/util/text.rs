//! Module responsible for rendering text.

use std::collections::HashSet;
use std::fmt;
use std::ops::{Add, Div, Sub};

use float_ord::FloatOrd;
use image::{DynamicImage, GenericImage};
use itertools::Itertools;
use num::One;
use regex::Regex;
use rusttype::{GlyphId, Font, point, Point, Rect, Scale};
use unreachable::unreachable;

use model::{Color, HAlign, VAlign, DEFAULT_TEXT_SIZE};


/// Check if given font has all the glyphs for given text.
pub fn check<'f, 's>(font: &'f Font<'f>, text: &'s str) {
    let mut missing = HashSet::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            continue;
        }
        let glyph = font.glyph(ch);
        if glyph.is_none() || glyph.unwrap().id() == GlyphId(0) {
            missing.insert(ch as u32);
        }
    }
    if !missing.is_empty() {
        warn!("Missing glyphs for {} codepoint(s): {}",
            missing.len(), missing.into_iter().format(", "));
    }
}


/// Alignment of text within a rectangle.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Alignment {
    pub vertical: VAlign,
    pub horizontal: HAlign,
}

impl Alignment {
    /// Create a new `Alignment` struct.
    #[inline]
    pub fn new(vertical: VAlign, horizontal: HAlign) -> Self {
        Alignment{vertical: vertical, horizontal: horizontal}
    }
}

impl From<(VAlign, HAlign)> for Alignment {
    fn from((v, h): (VAlign, HAlign)) -> Self {
        Alignment::new(v, h)
    }
}
impl From<(HAlign, VAlign)> for Alignment {
    fn from((h, v): (HAlign, VAlign)) -> Self {
        Alignment::new(v, h)
    }
}

impl fmt::Debug for Alignment {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "Alignment::{:?}{:?}", self.vertical, self.horizontal)
    }
}

impl Alignment {
    /// The origin point for this alignment within given rectangle.
    ///
    /// Returns one of nine possible points at the edges of the rectangle.
    pub fn origin_within<N>(&self, rect: Rect<N>) -> Point<N>
        where N: Copy + One + Add<Output=N> + Sub<Output=N> + Div<Output=N>
    {
        let two = N::one() + N::one();
        let x = match self.horizontal {
            HAlign::Left => rect.min.x,
            HAlign::Center => rect.min.x + rect.width() / two,
            HAlign::Right => rect.max.x,
        };
        let y = match self.vertical {
            VAlign::Top => rect.min.y,
            VAlign::Middle => rect.min.y + rect.height() / two,
            VAlign::Bottom => rect.max.y,
        };
        point(x, y)
    }
}


/// Style that the text is rendered with.
pub struct Style<'f> {
    font: &'f Font<'f>,
    size: f32,
    color: Color,
}

impl<'f> Style<'f> {
    /// Create a new `Style`.
    #[inline]
    pub fn new(font: &'f Font, size: f32, color: Color) -> Self {
        if size <= 0.0 {
            panic!("text::Style got negative size ({})", size);
        }
        Style{font, size, color}
    }

    /// Get a text `Scale` corresponding to the `Style`.
    #[inline]
    pub fn scale(&self) -> Scale {
        Scale::uniform(self.size)
    }

    /// Return the line height for a text in this style.
    pub fn line_height(&self) -> f32 {
        let v_metrics = self.font.v_metrics(self.scale());
        v_metrics.ascent + v_metrics.line_gap
    }
}

impl<'f> fmt::Debug for Style<'f> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Style")
            .field("font", &"Font{}")  // we don't have any displayable info here
            .field("size", &self.size)
            .field("color", &self.color)
            .finish()
    }
}


/// Renders text onto given image.
pub fn render_text<A: Into<Alignment>>(img: DynamicImage,
                                       s: &str,
                                       align: A, rect: Rect<f32>,
                                       style: Style) -> DynamicImage {
    let mut img = img;
    let align: Alignment = align.into();
    trace!("render_text(..., <length: {}>, {:?}, {:?}, {:?})",
        s.len(), align, rect, style);

    let mut lines = break_lines(s, &style, rect.width());
    trace!("Text broken into {} line(s)", lines.len());

    // TODO: do we need some adjustment for VAlign::Middle, too?
    if align.vertical == VAlign::Bottom {
        lines.reverse();
    }

    let mut rect = rect;
    let line_height = style.line_height();
    for line in lines {
        img = render_line(img, &line, align, rect, &style);

        // After rendering the line, shrink the rectangle by subtracting
        // line_height from its height in a way that plays well with vertical alignment.
        match align.vertical {
            VAlign::Top => rect.min.y += line_height,
            VAlign::Middle => {
                rect.min.y += line_height / 2.0;
                rect.max.y -= line_height / 2.0;
            }
            VAlign::Bottom => rect.max.y -= line_height,
        }
    }
    img
}

/// Renders a line of text onto given image.
///
/// Text should be single-line (line breaks are ignored)
/// and short enough to fit (or it will be clipped).
pub fn render_line<A: Into<Alignment>>(img: DynamicImage,
                                       s: &str,
                                       align: A, rect: Rect<f32>,
                                       style: &Style) -> DynamicImage {
    let mut img = img;
    let align: Alignment = align.into();
    trace!("render_line(..., {:?}, {:?}, {:?}, {:?})",
        s, align, rect, style);

    // Rendering text requires alpha blending.
    if img.as_rgba8().is_none() {
        img = DynamicImage::ImageRgba8(img.to_rgba());
    }

    let scale = style.scale();
    let v_metrics = style.font.v_metrics(scale);

    // Figure out where we're drawing.
    //
    // Unless it's a straightforward rendering in the top-left corner,
    // we need to compute the final bounds of the text first,
    // so that we can account for it when computing the start position.
    //
    let mut position = align.origin_within(rect);
    if align.horizontal != HAlign::Left {
        let width = text_width(s, &style);
        match align.horizontal {
            HAlign::Center => position.x -= width / 2.0,
            HAlign::Right => position.x -= width,
            _ => unsafe { unreachable(); },
        }
    }
    match align.vertical {
        VAlign::Top => position.y += v_metrics.ascent,
        VAlign::Middle => {
            let height = style.size;
            position.y += v_metrics.ascent - height / 2.0;
        },
        VAlign::Bottom => {
            position.y -= v_metrics.descent.abs();  // it's usually negative
        },
    }

    // Now we can draw the text.
    for glyph in style.font.layout(s, scale, position) {
        if let Some(bbox) = glyph.pixel_bounding_box() {
            glyph.draw(|x, y, v| {
                let x = (bbox.min.x + x as i32) as u32;
                let y = (bbox.min.y + y as i32) as u32;
                let alpha = (v * 255f32) as u8;
                if img.in_bounds(x, y) {
                    img.blend_pixel(x, y, style.color.to_rgba(alpha));
                }
            });
        }
    }

    img
}


/// Return the maximum text size that'd still allow us to fit the text
/// within given rectangle.
///
/// The size returned may be ridiculous if the text is long enough
/// (or the rectangle is small enough). However, if the size cannot be determined
/// in reasonable number of iterations, None is returned.
pub fn fit_text<'s, 'f>(rect: Rect<f32>, s: &'s str, font: &'f Font<'f>) -> Option<f32> {
    trace!("fit_text({:?}, <{} bytes of text>, ...", rect, s.len());
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }

    // TODO: pick a larger default size so that short texts will
    // still completely fill larger rectangles
    let mut size = DEFAULT_TEXT_SIZE;
    let unused_color = Color::white();  // not used, but needed for Style

    // Gradually shrink the text, break it into lines,
    // and try to fit it within the given rectangle.
    ///
    // Continue to do so until succeeded,
    // or a maximum number of iterations has been reached.
    const SHRINK_FACTOR: f32 = 0.9;
    const MAX_ITERS: usize = 16;
    let mut iters = 1;
    while iters <= MAX_ITERS {
        let style = Style::new(font, size, unused_color);
        let lines = break_lines(s, &style, rect.width());

        let width = lines.iter().map(|line| text_width(line, &style))
            .map(FloatOrd).max().map(|w| w.0)
            .unwrap_or(0.0);
        let height = lines.len() as f32 * style.line_height();
        if width <= rect.width() && height <= rect.height() {
            break;  // Found a fitting size.
        }

        let new_size = size * SHRINK_FACTOR;
        if new_size >= size {
            // Seems we got REALLY small and float inaccuracies started to matter.
            warn!("Text size lost accuracy ({:?}) after {} iterations, starting from size {}",
                new_size, iters, DEFAULT_TEXT_SIZE);
            return None;
        }
        size = new_size;
        iters += 1;
    }

    if iters > MAX_ITERS {
        warn!(
            "Couldn't fit text in a {}x{} rect even after {} iterations (last attempt: {})",
            rect.width(), rect.height(), MAX_ITERS, size);
        return None;
    }
    Some(size)
}

/// Return the maximum text size that'd still allow us to fit a line
/// within given maximum width.
///
/// The size returned may be ridiculous if the text is long enough
/// (or max_width is low enough). However, if the size cannot be determined
/// in reasonable number of iterations, None is returned.
///
/// This should only be called on single-line texts.
/// Any preexisting line break characters will be ignored.
pub fn fit_line<'s, 'f>(max_width: f32, s: &'s str, font: &'f Font<'f>) -> Option<f32> {
    trace!("fit_line({:?}, <{} bytes of text>, ...)", max_width, s.len());
    if max_width <= 0.0 {
        return None;
    }

    // TODO: pick a larger default size so that short texts will
    // still completely fill larger rectangles
    let mut size = DEFAULT_TEXT_SIZE;
    let color = Color::white();  // not used, but needed for Style

    // Gradually shrink the size and try to fit it,
    // but prevent infinite loops if we can't fit it after all.
    const MAX_ITERS: usize = 16;
    let mut iters = 1;
    while iters <= MAX_ITERS && text_width(s, &Style::new(font, size, color)) > max_width {
        const SHRINK_FACTOR: f32 = 0.9;
        let new_size = size * SHRINK_FACTOR;
        if new_size >= size {
            // Seems we got REALLY small and float inaccuracies started to matter.
            warn!("Text size lost accuracy ({:?}) after {} iterations, starting from {}",
                new_size, iters, DEFAULT_TEXT_SIZE);
            return None;
        }
        size = new_size;
        iters += 1;
    }

    if iters > MAX_ITERS {
        warn!(
            "Couldn't fit text in a width of {} even after {} iterations (last attempt: {})",
            max_width, MAX_ITERS, size);
        return None;
    }
    Some(size)
}


// Text measurement.

/// Compute the pixel width of given text.
fn text_width(s: &str, style: &Style) -> f32 {
    // Compute text width as the final X position of the "caret"
    // after laying out all the glyphs, starting from X=0.
    let glyphs: Vec<_> = style.font
        .layout(s, style.scale(), point(0.0, /* unused */ 0.0))
        .collect();
    glyphs.iter()
        .rev()
        .filter_map(|g| g.pixel_bounding_box().map(|bb| {
            bb.min.x as f32 + g.unpositioned().h_metrics().advance_width
        }))
        .next().unwrap_or(0.0)
}

/// Compute the pixel width of given character.
fn char_width(c: char, style: &Style) -> f32 {
    // This isn't just text_width() call for a 1-char string,
    // because the result would include a bounding box shift used for kerning.
    style.font.glyph(c)
        .map(|g| g.scaled(style.scale()).h_metrics().advance_width)
        .unwrap_or(0.0)
}


// Line breaking.

/// Break the text into lines, fitting given width.
fn break_lines(s: &str, style: &Style, line_width: f32) -> Vec<String> {
    s.lines()
        .flat_map(|line| break_single_line(line, style, line_width))
        .collect()
}

/// Break a single line into multiple lines.
/// The line should not contain explicit line breaks.
fn break_single_line(s: &str, style: &Style, line_width: f32) -> Vec<String> {
    lazy_static! {
        static ref WORD_BOUNDARY: Regex = Regex::new(r"\b").unwrap();
    }

    let segments: Vec<&str> = WORD_BOUNDARY.split(s).filter(|s| !s.is_empty()).collect();
    let is_word = |s: &str| s.chars().all(|c| !c.is_whitespace());
    trace!("Computing line breaks for text of length {} with {} word(s) and {} gap(s)",
        s.len(),
        segments.iter().map(|s| is_word(s)).count(),
        segments.iter().map(|s| !is_word(s)).count());

    let mut result = Vec::with_capacity(segments.len() / 2 /* a guess */);

    let mut current_line = String::new();
    let mut current_width = 0.0;
    for segment in segments {
        let mut segment_width = text_width(segment, style);

        // Simplest case is when the segment trivially fits within the line.
        if current_width + segment_width < line_width {
            current_line.push_str(segment);
            current_width += segment_width;
            continue;
        }

        // If the segment doesn't fit, but it is not longer than the line by itself,
        // break the current line before it & put the segment in the next one.
        if segment_width < line_width {
            if !current_line.is_empty() {
                result.push(current_line);
            }
            // If the overflowing segment is just a single space,
            // then just forget about it completely.
            // That space is adequately represented by the line break itself.
            if segment == " " {
                current_line = String::new();
                current_width = 0.0;
            } else {
                current_line = segment.to_owned();
                current_width = segment_width;
            }
            continue;
        }

        // The worst case scenario is that the segment itself is longer than the line.
        // In this case, we have to break it up (possibly multiple times).
        let mut segment = segment.to_owned();
        loop {
            // Break it at the earliest possible spot by shaving off characters
            // from the end. Remember what part of the segment shall carry over
            // to the next line, too.
            let mut carryover: Vec<char> = vec![];
            let mut carryover_width = 0.0;
            while current_width + segment_width > line_width {
                match segment.pop() {
                    Some(c) => {
                        carryover.push(c);
                        let ch_width = char_width(c, style);
                        segment_width -= ch_width;
                        carryover_width += ch_width;
                    },
                    None => {
                        segment_width = 0.0;
                        break;
                    },
                }
            }

            // What remains will fit within the current line now,
            // so we just add it in there.
            // And if there is nothing to carry over, we're done.
            current_line.push_str(&segment);
            current_width += segment_width;
            if carryover.is_empty() {
                break;
            }

            // Otherwise, we need to start a new line for the carryover part...
            result.push(current_line);
            current_line = String::new();
            current_width = 0.0;

            // ...which now also becomes the new segment part,
            // ready to be broken up in an identical way.
            segment = carryover.into_iter().rev().collect();
            segment_width = carryover_width;
        }
    }
    if !current_line.is_empty() {
        result.push(current_line);
    }

    result
}
