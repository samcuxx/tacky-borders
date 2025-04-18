use anyhow::{Context, anyhow};
use core::f32;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;
use windows::Win32::Foundation::{FALSE, RECT};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BRUSH_PROPERTIES, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, ID2D1Brush, ID2D1LinearGradientBrush, ID2D1RenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::core::BOOL;
use windows_numerics::{Matrix3x2, Vector2};

use crate::LogIfErr;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ColorBrushConfig {
    Solid(String),
    Gradient(GradientBrushConfig),
}

impl Default for ColorBrushConfig {
    fn default() -> Self {
        Self::Solid("accent".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GradientBrushConfig {
    pub colors: Vec<String>,
    pub direction: GradientDirection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum GradientDirection {
    Angle(String),
    Coordinates(GradientCoordinates),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GradientCoordinates {
    pub start: [f32; 2],
    pub end: [f32; 2],
}

#[derive(Debug, Clone)]
pub enum ColorBrush {
    Solid(SolidBrush),
    Gradient(GradientBrush),
}

impl Default for ColorBrush {
    fn default() -> Self {
        ColorBrush::Solid(SolidBrush {
            color: D2D1_COLOR_F::default(),
            brush: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SolidBrush {
    color: D2D1_COLOR_F,
    brush: Option<ID2D1SolidColorBrush>,
}

#[derive(Debug, Clone)]
pub struct GradientBrush {
    gradient_stops: Vec<D2D1_GRADIENT_STOP>,
    direction: GradientCoordinates,
    brush: Option<ID2D1LinearGradientBrush>,
}

impl ColorBrushConfig {
    pub fn to_color_brush(&self, is_active_color: bool) -> ColorBrush {
        match self {
            ColorBrushConfig::Solid(solid_config) => {
                if solid_config == "accent" {
                    ColorBrush::Solid(SolidBrush {
                        color: get_accent_color(is_active_color),
                        brush: None,
                    })
                } else {
                    ColorBrush::Solid(SolidBrush {
                        color: get_color_from_hex(solid_config.as_str()),
                        brush: None,
                    })
                }
            }
            ColorBrushConfig::Gradient(gradient_config) => {
                // We use 'step' to calculate the position of each color in the gradient below
                let step = 1.0 / (gradient_config.colors.len() - 1) as f32;

                let gradient_stops = gradient_config
                    .clone()
                    .colors
                    .into_iter()
                    .enumerate()
                    .map(|(i, color)| D2D1_GRADIENT_STOP {
                        position: i as f32 * step,
                        color: if color == "accent" {
                            get_accent_color(is_active_color)
                        } else {
                            get_color_from_hex(color.as_str())
                        },
                    })
                    .collect();

                let direction = match gradient_config.direction {
                    // We'll convert an angle to coordinates by representing the angle as a linear
                    // line, then checking for collisions within the unit square bounded by (0.0,
                    // 0.0) and (1.0, 1.0)
                    GradientDirection::Angle(ref angle) => {
                        let Some(degree) = angle
                            .strip_suffix("deg")
                            .and_then(|d| d.trim().parse::<f32>().ok())
                        else {
                            error!("config contains an invalid gradient direction!");
                            return ColorBrush::default();
                        };

                        // Convert degrees to radians. We multiply `degree` by -1 because Direct2D
                        // uses the top left for the origin instead of the bottom left
                        let rad = -degree * PI / 180.0;

                        // Calculate the slope of the line. We also handle edge cases like 90
                        // degrees or 270 degrees to avoid division by 0.
                        let m = match degree.abs() % 360.0 {
                            90.0 | 270.0 => degree.signum() * f32::MAX,
                            _ => rad.sin() / rad.cos(),
                        };

                        // y - y_p = m(x - x_p);
                        // y = m(x - x_p) + y_p;
                        // y = (m * x) - (m * x_p) + y_p;
                        // b = -(m * x_p) + y_p;
                        //
                        // Calculate the y-intercept of the line such that it goes through the
                        // center point (0.5, 0.5)
                        let b = -m * 0.5 + 0.5;

                        // Create the line with the given slope and y-intercept
                        let line = Line { m, b };

                        // Determine initial x-value estimates for the start and end points
                        let (x_s, x_e) = match degree.abs() % 360.0 {
                            0.0..90.0 => (0.0, 1.0),
                            90.0..270.0 => (1.0, 0.0),
                            270.0..360.0 => (0.0, 1.0),
                            _ => {
                                debug!(
                                    "reached a gradient angle that is not covered by the match statement in colors.rs"
                                );
                                (0.0, 1.0)
                            }
                        };

                        // y = mx + b
                        // 0 = mx + b
                        // mx = -b
                        // x = -b/m
                        //
                        // y = mx + b
                        // 1 = mx + b
                        // mx = 1 - b
                        // x = (1 - b)/m
                        //
                        // Determine our coordinates by checking collisions with the unit square
                        // using the above x_s and x_e, handling three separate cases:
                        //  1. the y-coordinate at x_s/x_e is between 0 and 1
                        //  2. the y-coordinate at x_s/x_e is greater than 1
                        //  3. the y-coordinate at x_s/x_e is less than 0
                        let start = match line.plug_in_x(x_s) {
                            0.0..=1.0 => [x_s, line.plug_in_x(x_s)], // Case 1
                            1.0.. => [(1.0 - line.b) / line.m, 1.0], // Case 2
                            _ => [-line.b / line.m, 0.0],            // Case 3
                        };

                        let end = match line.plug_in_x(x_e) {
                            0.0..=1.0 => [x_e, line.plug_in_x(x_e)], // Case 1
                            1.0.. => [(1.0 - line.b) / line.m, 1.0], // Case 2
                            _ => [-line.b / line.m, 0.0],            // Case 3
                        };

                        GradientCoordinates { start, end }
                    }
                    GradientDirection::Coordinates(ref coordinates) => coordinates.clone(),
                };

                ColorBrush::Gradient(GradientBrush {
                    gradient_stops,
                    direction,
                    brush: None,
                })
            }
        }
    }
}

#[derive(Debug)]
struct Line {
    m: f32,
    b: f32,
}

impl Line {
    fn plug_in_x(&self, x: f32) -> f32 {
        self.m * x + self.b
    }
}

impl ColorBrush {
    // NOTE: ID2D1DeviceContext implements From<&ID2D1DeviceContext> for &ID2D1RenderTarget
    pub fn init_brush(
        &mut self,
        renderer: &ID2D1RenderTarget,
        window_rect: &RECT,
        brush_properties: &D2D1_BRUSH_PROPERTIES,
    ) -> windows::core::Result<()> {
        match self {
            ColorBrush::Solid(solid) => unsafe {
                let id2d1_brush =
                    renderer.CreateSolidColorBrush(&solid.color, Some(brush_properties))?;

                solid.brush = Some(id2d1_brush);

                Ok(())
            },
            ColorBrush::Gradient(gradient) => unsafe {
                let width = (window_rect.right - window_rect.left) as f32;
                let height = (window_rect.bottom - window_rect.top) as f32;

                // The direction/GradientCoordinates only range from 0.0 to 1.0, but we need to
                // convert it into coordinates in terms of the screen's pixels
                let gradient_properties = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 {
                        X: gradient.direction.start[0] * width,
                        Y: gradient.direction.start[1] * height,
                    },
                    endPoint: Vector2 {
                        X: gradient.direction.end[0] * width,
                        Y: gradient.direction.end[1] * height,
                    },
                };

                let gradient_stop_collection = renderer.CreateGradientStopCollection(
                    &gradient.gradient_stops,
                    D2D1_GAMMA_2_2,
                    D2D1_EXTEND_MODE_CLAMP,
                )?;

                let id2d1_brush = renderer.CreateLinearGradientBrush(
                    &gradient_properties,
                    Some(brush_properties),
                    &gradient_stop_collection,
                )?;

                gradient.brush = Some(id2d1_brush);

                Ok(())
            },
        }
    }

    pub fn get_brush(&self) -> Option<&ID2D1Brush> {
        match self {
            ColorBrush::Solid(solid) => solid.brush.as_ref().map(|id2d1_brush| id2d1_brush.into()),
            ColorBrush::Gradient(gradient) => gradient
                .brush
                .as_ref()
                .map(|id2d1_brush| id2d1_brush.into()),
        }
    }

    pub fn set_opacity(&self, opacity: f32) -> anyhow::Result<()> {
        match self {
            ColorBrush::Solid(solid) => {
                let id2d1_brush = solid
                    .brush
                    .as_ref()
                    .context("brush has not been created yet")?;

                unsafe { id2d1_brush.SetOpacity(opacity) };
            }
            ColorBrush::Gradient(gradient) => {
                let id2d1_brush = gradient
                    .brush
                    .as_ref()
                    .context("brush has not been created yet")?;

                unsafe { id2d1_brush.SetOpacity(opacity) };
            }
        }

        Ok(())
    }

    pub fn get_opacity(&self) -> anyhow::Result<f32> {
        match self {
            ColorBrush::Solid(solid) => {
                let id2d1_brush = solid
                    .brush
                    .as_ref()
                    .context("brush has not been created yet")?;

                Ok(unsafe { id2d1_brush.GetOpacity() })
            }
            ColorBrush::Gradient(gradient) => {
                let id2d1_brush = gradient
                    .brush
                    .as_ref()
                    .context("brush has not been created yet")?;

                Ok(unsafe { id2d1_brush.GetOpacity() })
            }
        }
    }

    pub fn set_transform(&self, transform: &Matrix3x2) {
        match self {
            ColorBrush::Solid(solid) => {
                if let Some(ref id2d1_brush) = solid.brush {
                    unsafe { id2d1_brush.SetTransform(transform) };
                }
            }
            ColorBrush::Gradient(gradient) => {
                if let Some(ref id2d1_brush) = gradient.brush {
                    unsafe { id2d1_brush.SetTransform(transform) };
                }
            }
        }
    }

    pub fn get_transform(&self) -> Option<Matrix3x2> {
        match self {
            ColorBrush::Solid(solid) => solid.brush.as_ref().map(|id2d1_brush| {
                let mut transform = Matrix3x2::default();
                unsafe { id2d1_brush.GetTransform(&mut transform) };

                transform
            }),
            ColorBrush::Gradient(gradient) => gradient.brush.as_ref().map(|id2d1_brush| {
                let mut transform = Matrix3x2::default();
                unsafe { id2d1_brush.GetTransform(&mut transform) };

                transform
            }),
        }
    }
}

impl GradientBrush {
    pub fn update_start_end_points(&self, window_rect: &RECT) {
        let width = (window_rect.right - window_rect.left) as f32;
        let height = (window_rect.bottom - window_rect.top) as f32;

        // The direction/GradientCoordinates only range from 0.0 to 1.0, but we need to
        // convert it into coordinates in terms of pixels
        let start_point = Vector2 {
            X: self.direction.start[0] * width,
            Y: self.direction.start[1] * height,
        };
        let end_point = Vector2 {
            X: self.direction.end[0] * width,
            Y: self.direction.end[1] * height,
        };

        if let Some(ref id2d1_brush) = self.brush {
            unsafe {
                id2d1_brush.SetStartPoint(start_point);
                id2d1_brush.SetEndPoint(end_point)
            };
        }
    }
}

fn get_accent_color(is_active_color: bool) -> D2D1_COLOR_F {
    let mut pcr_colorization: u32 = 0;
    let mut pf_opaqueblend: BOOL = FALSE;

    // DwmGetColorizationColor gets the accent color and places it into 'pcr_colorization'
    unsafe { DwmGetColorizationColor(&mut pcr_colorization, &mut pf_opaqueblend) }
        .context("could not retrieve windows accent color")
        .log_if_err();

    // Bit-shift the retrieved color to separate out the rgb components
    let accent_red = ((pcr_colorization & 0x00FF0000) >> 16) as f32 / 255.0;
    let accent_green = ((pcr_colorization & 0x0000FF00) >> 8) as f32 / 255.0;
    let accent_blue = (pcr_colorization & 0x000000FF) as f32 / 255.0;
    let accent_avg = (accent_red + accent_green + accent_blue) / 3.0;

    if is_active_color {
        D2D1_COLOR_F {
            r: accent_red,
            g: accent_green,
            b: accent_blue,
            a: 1.0,
        }
    } else {
        D2D1_COLOR_F {
            r: accent_avg / 1.5 + accent_red / 10.0,
            g: accent_avg / 1.5 + accent_green / 10.0,
            b: accent_avg / 1.5 + accent_blue / 10.0,
            a: 1.0,
        }
    }
}

fn get_color_from_hex(hex: &str) -> D2D1_COLOR_F {
    let s = hex.strip_prefix("#").unwrap_or_default();
    parse_hex(s).unwrap_or_else(|err| {
        error!("could not parse hex: {err}");
        D2D1_COLOR_F::default()
    })
}

fn parse_hex(s: &str) -> anyhow::Result<D2D1_COLOR_F> {
    if !matches!(s.len(), 3 | 4 | 6 | 8) || !s[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid hex: {s}"));
    }

    let n = s.len();

    let parse_digit = |digit: &str, single: bool| -> anyhow::Result<f32> {
        u8::from_str_radix(digit, 16)
            .map(|n| {
                if single {
                    ((n << 4) | n) as f32 / 255.0
                } else {
                    n as f32 / 255.0
                }
            })
            .map_err(|_| anyhow!("invalid hex: {s}"))
    };

    if n == 3 || n == 4 {
        let r = parse_digit(&s[0..1], true)?;
        let g = parse_digit(&s[1..2], true)?;
        let b = parse_digit(&s[2..3], true)?;

        let a = if n == 4 {
            parse_digit(&s[3..4], true)?
        } else {
            1.0
        };

        Ok(D2D1_COLOR_F { r, g, b, a })
    } else if n == 6 || n == 8 {
        let r = parse_digit(&s[0..2], false)?;
        let g = parse_digit(&s[2..4], false)?;
        let b = parse_digit(&s[4..6], false)?;

        let a = if n == 8 {
            parse_digit(&s[6..8], false)?
        } else {
            1.0
        };

        Ok(D2D1_COLOR_F { r, g, b, a })
    } else {
        Err(anyhow!("invalid hex: {s}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vertical_gradient_90() -> anyhow::Result<()> {
        let color_brush_config = ColorBrushConfig::Gradient(GradientBrushConfig {
            colors: vec!["#ffffff".to_string(), "#000000".to_string()],
            direction: GradientDirection::Angle("90deg".to_string()),
        });
        let color_brush = color_brush_config.to_color_brush(true);

        if let ColorBrush::Gradient(ref gradient) = color_brush {
            assert!(gradient.direction.start == [0.5, 1.0]);
            assert!(gradient.direction.end == [0.5, 0.0]);
        } else {
            panic!("created incorrect color brush");
        }

        Ok(())
    }

    #[test]
    fn test_vertical_gradient_neg90() -> anyhow::Result<()> {
        let color_brush_config = ColorBrushConfig::Gradient(GradientBrushConfig {
            colors: vec!["#ffffff".to_string(), "#000000".to_string()],
            direction: GradientDirection::Angle("-90deg".to_string()),
        });
        let color_brush = color_brush_config.to_color_brush(true);

        if let ColorBrush::Gradient(ref gradient) = color_brush {
            assert!(gradient.direction.start == [0.5, 0.0]);
            assert!(gradient.direction.end == [0.5, 1.0]);
        } else {
            panic!("created incorrect color brush");
        }

        Ok(())
    }

    #[test]
    fn test_gradient_excess_angle() -> anyhow::Result<()> {
        let color_brush_config = ColorBrushConfig::Gradient(GradientBrushConfig {
            colors: vec!["#ffffff".to_string(), "#000000".to_string()],
            direction: GradientDirection::Angle("-540deg".to_string()),
        });
        let color_brush = color_brush_config.to_color_brush(true);

        if let ColorBrush::Gradient(ref gradient) = color_brush {
            assert!(gradient.direction.start == [1.0, 0.5]);
            assert!(gradient.direction.end == [0.0, 0.5]);
        } else {
            panic!("created incorrect color brush");
        }

        Ok(())
    }

    #[test]
    fn test_color_parser_translucent() -> anyhow::Result<()> {
        let color_brush_config = ColorBrushConfig::Solid("#ffffff80".to_string());
        let color_brush = color_brush_config.to_color_brush(true);

        if let ColorBrush::Solid(ref solid) = color_brush {
            assert!(
                solid.color
                    == D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 128.0 / 255.0
                    }
            );
        } else {
            panic!("created incorrect color brush");
        }

        Ok(())
    }
}
