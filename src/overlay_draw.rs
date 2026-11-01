//! 録音中の表示の絵。Windows と macOS で共通の、1 ピクセル 1 つの画像への描画
//! 文字の形だけは各 OS の仕組みで作り、`TextMask` として受け取る

use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Recording { started: Instant },
    Busy { label: String, hint: Option<String> },
    Notice { tone: Tone, text: String },
}

pub const FADE_IN: Duration = Duration::from_millis(140);
pub const FADE_OUT: Duration = Duration::from_millis(200);
pub const FRAME: Duration = Duration::from_millis(16);
pub const BARS: usize = 5;

pub fn ease_out(p: f32) -> f32 {
    1.0 - (1.0 - p).powi(3)
}

pub fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// -50dB で 0、-10dB で 1 になるよう対数で割り当てる。人の声は -35〜-15dB あたりに来る
pub fn level_from_rms(rms: f32) -> f32 {
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 50.0) / 40.0).clamp(0.0, 1.0)
}

#[derive(Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub struct Palette {
    pub bg: Rgb,
    pub bg_alpha: f32,
    pub edge: Rgb,
    pub edge_alpha: f32,
    pub fg: Rgb,
    pub red: Rgb,
    pub amber: Rgb,
    pub shadow_alpha: f32,
}

pub const DARK: Palette = Palette {
    bg: Rgb(24, 24, 27),
    bg_alpha: 0.92,
    edge: Rgb(255, 255, 255),
    edge_alpha: 0.12,
    fg: Rgb(255, 255, 255),
    red: Rgb(255, 69, 58),
    amber: Rgb(255, 179, 64),
    shadow_alpha: 0.30,
};

// 白地に赤い点で日の丸。赤は国旗の色(#BC002D)
pub const HINOMARU: Palette = Palette {
    bg: Rgb(252, 252, 250),
    bg_alpha: 0.97,
    edge: Rgb(0, 0, 0),
    edge_alpha: 0.10,
    fg: Rgb(28, 28, 30),
    red: Rgb(188, 0, 45),
    amber: Rgb(214, 124, 0),
    shadow_alpha: 0.22,
};

/// "auto" は OS のアプリ用のライト、ダークに合わせる。それ以外はすべて日の丸
pub fn palette_for(theme: &str, system_light: impl FnOnce() -> bool) -> &'static Palette {
    if theme == "auto" && !system_light() {
        &DARK
    } else {
        &HINOMARU
    }
}

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<[f32; 4]>, // 乗算済みの b, g, r, a(0〜1)
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Canvas {
        Canvas {
            w,
            h,
            px: vec![[0.0; 4]; (w * h) as usize],
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: Rgb, alpha: f32) {
        if alpha <= 0.0 || x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let a = alpha.min(1.0);
        let p = &mut self.px[(y * self.w + x) as usize];
        let src = [
            c.2 as f32 / 255.0 * a,
            c.1 as f32 / 255.0 * a,
            c.0 as f32 / 255.0 * a,
            a,
        ];
        for i in 0..4 {
            p[i] = src[i] + p[i] * (1.0 - a);
        }
    }

    /// 角丸の長方形を符号付き距離で塗る。縁は 1 ピクセル幅でなめらかにする
    #[allow(clippy::too_many_arguments)] // Drawing primitive keeps explicit geometry.
    fn round_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
        c: Rgb,
        alpha: f32,
        soften: f32,
    ) {
        let (x0, y0) = (
            (x - soften - 1.0).floor() as i32,
            (y - soften - 1.0).floor() as i32,
        );
        let (x1, y1) = (
            (x + w + soften + 1.0).ceil() as i32,
            (y + h + soften + 1.0).ceil() as i32,
        );
        for py in y0..y1 {
            for px in x0..x1 {
                let d = round_rect_sdf(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, r);
                let cov = coverage(d, soften);
                self.blend(px, py, c, alpha * cov);
            }
        }
    }

    /// 角丸の長方形の内側 1 ピクセルの縁だけを塗る
    #[allow(clippy::too_many_arguments)] // Drawing primitive keeps explicit geometry.
    fn stroke_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb, alpha: f32) {
        for py in (y.floor() as i32)..((y + h).ceil() as i32) {
            for px in (x.floor() as i32)..((x + w).ceil() as i32) {
                let d = round_rect_sdf(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, r);
                self.blend(
                    px,
                    py,
                    c,
                    alpha * (coverage(d, 0.0) - coverage(d + 1.0, 0.0)),
                );
            }
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Rgb, alpha: f32) {
        self.round_rect(cx - r, cy - r, r * 2.0, r * 2.0, r, c, alpha, 0.0);
    }

    fn text(&mut self, mask: &TextMask, x: i32, y: i32, c: Rgb, alpha: f32) {
        for ty in 0..mask.h {
            for tx in 0..mask.w {
                let cov = mask.cov[(ty * mask.w + tx) as usize] as f32 / 255.0;
                self.blend(x + tx, y + ty, c, alpha * cov);
            }
        }
    }
}

pub fn round_rect_sdf(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let qx = (px - cx).abs() - (w / 2.0 - r);
    let qy = (py - cy).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// 距離 d(内側が負)から塗りの割合を出す。soften > 0 なら影のようにぼかす
pub fn coverage(d: f32, soften: f32) -> f32 {
    if soften > 0.0 {
        let t = (1.0 - d / soften).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    } else {
        (0.5 - d).clamp(0.0, 1.0)
    }
}

pub struct TextMask {
    pub w: i32,
    pub h: i32,
    pub cov: Vec<u8>,
}

/// 表示 1 枚を描く。文字は render_text(文字, 高さ px, 太字か, 最大幅 px) で作る
pub fn compose(
    mode: &Mode,
    pal: &Palette,
    s: f32,
    level: f32,
    render_text: impl Fn(&str, i32, bool, i32) -> TextMask,
) -> Canvas {
    let px = |v: f32| (v * s).round();
    let shadow = px(12.0);
    let pill_h = px(36.0);
    let pad = px(14.0);
    let gap = px(9.0);
    let font_px = px(13.0) as i32;
    let max_text = px(440.0) as i32;

    // 中身の幅を先に決める
    let (label, hint, dot): (Option<TextMask>, Option<TextMask>, Option<Rgb>) = match mode {
        Mode::Recording { started } => {
            let secs = started.elapsed().as_secs();
            (
                Some(render_text(
                    &format!("{}:{:02}", secs / 60, secs % 60),
                    font_px,
                    true,
                    max_text,
                )),
                None,
                None,
            )
        }
        Mode::Busy { label, hint } => (
            Some(render_text(label, font_px, true, max_text)),
            hint.as_deref()
                .map(|h| render_text(h, font_px, false, max_text)),
            None,
        ),
        Mode::Notice { tone, text } => (
            Some(render_text(text, font_px, true, max_text)),
            None,
            Some(if *tone == Tone::Warn {
                pal.amber
            } else {
                pal.red
            }),
        ),
    };
    let lead_w = match mode {
        Mode::Recording { .. } => {
            px(10.0) + gap + px(3.0) * BARS as f32 + px(3.0) * (BARS as f32 - 1.0)
        }
        Mode::Busy { .. } => px(22.0),
        Mode::Notice { .. } => px(8.0),
    };
    let label_w = label.as_ref().map_or(0, |m| m.w) as f32;
    let hint_w = hint.as_ref().map_or(0.0, |m| m.w as f32 + px(6.0));
    let pill_w = (pad * 2.0 + lead_w + gap + label_w + hint_w).max(px(96.0));

    let w = (pill_w + shadow * 2.0) as i32;
    let h = (pill_h + shadow * 2.0) as i32;
    let mut c = Canvas::new(w, h);
    let (x0, y0) = (shadow, shadow);

    // 影、地、縁取り
    c.round_rect(
        x0,
        y0 + px(2.0),
        pill_w,
        pill_h,
        pill_h / 2.0,
        Rgb(0, 0, 0),
        pal.shadow_alpha,
        shadow,
    );
    c.round_rect(
        x0,
        y0,
        pill_w,
        pill_h,
        pill_h / 2.0,
        pal.bg,
        pal.bg_alpha,
        0.0,
    );
    c.stroke_round_rect(
        x0,
        y0,
        pill_w,
        pill_h,
        pill_h / 2.0,
        pal.edge,
        pal.edge_alpha,
    );

    let cy = y0 + pill_h / 2.0;
    let mut x = x0 + pad;
    let phase = start_phase();
    match mode {
        Mode::Recording { .. } => {
            // 脈打つ赤い点。外側の輪が広がって消える
            let p = (phase % 1.4) / 1.4;
            c.circle(
                x + px(5.0),
                cy,
                px(5.0) + px(5.0) * p,
                pal.red,
                0.35 * (1.0 - p),
            );
            c.circle(x + px(5.0), cy, px(5.0), pal.red, 1.0);
            x += px(10.0) + gap;
            // 音量の棒。実際のマイクの音量で伸び、各棒を少しずつずらして揺らす
            let shape = [0.55, 0.8, 1.0, 0.8, 0.55];
            for (i, k) in shape.iter().enumerate() {
                let wobble = 0.75 + 0.25 * ((phase * 9.0 + i as f32 * 1.7).sin() * 0.5 + 0.5);
                let bh = px(4.0) + (px(18.0) - px(4.0)) * (level * k * wobble).min(1.0);
                c.round_rect(x, cy - bh / 2.0, px(3.0), bh, px(1.5), pal.fg, 0.9, 0.0);
                x += px(6.0);
            }
            x += gap - px(3.0);
        }
        Mode::Busy { .. } => {
            // 3 つの点が順に明るくなる
            for i in 0..3 {
                let a = 0.3 + 0.7 * (((phase * 5.0 - i as f32 * 0.9).sin() * 0.5 + 0.5).powi(2));
                c.circle(x + px(3.0) + px(8.0) * i as f32, cy, px(2.6), pal.fg, a);
            }
            x += px(22.0) + gap;
        }
        Mode::Notice { .. } => {
            if let Some(color) = dot {
                c.circle(x + px(4.0), cy, px(4.0), color, 1.0);
            }
            x += px(8.0) + gap;
        }
    }
    if let Some(m) = &label {
        c.text(m, x as i32, (cy - m.h as f32 / 2.0) as i32, pal.fg, 0.95);
        x += m.w as f32 + px(6.0);
    }
    if let Some(m) = &hint {
        c.text(m, x as i32, (cy - m.h as f32 / 2.0) as i32, pal.fg, 0.5);
    }
    c
}

pub fn start_phase() -> f32 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f32()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_maps_voice_range_into_meter() {
        assert_eq!(level_from_rms(0.0), 0.0);
        assert_eq!(level_from_rms(1.0), 1.0);
        let quiet_room = level_from_rms(0.002); // 約 -54dB
        let speech = level_from_rms(0.05); // 約 -26dB
        assert!(quiet_room < 0.05, "{quiet_room}");
        assert!(speech > 0.5 && speech < 0.7, "{speech}");
    }

    #[test]
    fn round_rect_edges_are_antialiased() {
        // 中心は塗り、遠くは塗らず、縁はその中間
        assert_eq!(
            coverage(round_rect_sdf(50.0, 18.0, 0.0, 0.0, 100.0, 36.0, 18.0), 0.0),
            1.0
        );
        assert_eq!(
            coverage(
                round_rect_sdf(150.0, 18.0, 0.0, 0.0, 100.0, 36.0, 18.0),
                0.0
            ),
            0.0
        );
        let edge = coverage(
            round_rect_sdf(100.0, 18.0, 0.0, 0.0, 100.0, 36.0, 18.0),
            0.0,
        );
        assert!(edge > 0.4 && edge < 0.6, "{edge}");
        // 角は丸く抜けている
        assert_eq!(
            coverage(round_rect_sdf(0.5, 0.5, 0.0, 0.0, 100.0, 36.0, 18.0), 0.0),
            0.0
        );
    }

    #[test]
    fn premultiplied_blend_over_transparent() {
        let mut c = Canvas::new(1, 1);
        c.blend(0, 0, Rgb(255, 0, 0), 0.5);
        let p = c.px[0];
        assert!((p[2] - 0.5).abs() < 1e-6 && p[0] == 0.0 && (p[3] - 0.5).abs() < 1e-6);
    }
}
