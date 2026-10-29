//! QODA logosu: `assets/qoda-logo.svg` ile aynı geometri, uzaklık alanlarıyla
//! (kenar yumuşatmalı) yazılım içinde çizilir. Pencere simgesi ve üst bardaki
//! logo bu çizimden üretilir; böylece ek görüntü dosyası gerekmez.

/// SVG'nin 256 birimlik koordinat sistemindeki geometri.
const VIEW: f32 = 256.0;
const RING_CENTER: (f32, f32) = (124.0, 124.0);
const RING_RADIUS: f32 = 70.0;
const STROKE: f32 = 22.0;
/// Q'nun kuyruğu halkanın içinden başlayıp halkayı keser; koyu kontur onu halkadan ayırır.
const TAIL: ((f32, f32), (f32, f32)) = ((156.0, 156.0), (206.0, 206.0));
const TAIL_OUTLINE: f32 = 6.0;
const SATELLITE: (f32, f32) = (173.5, 74.5);
const SATELLITE_RADIUS: f32 = 14.0;
const SATELLITE_OUTLINE: f32 = 7.0;
const PLAY: [(f32, f32); 3] = [(110.0, 99.0), (110.0, 149.0), (152.0, 124.0)];
const PLAY_ROUNDING: f32 = 4.0;

const BACKGROUND: [f32; 3] = [11.0, 15.0, 22.0];
const CYAN: [f32; 3] = [56.0, 200.0, 240.0];
const AMBER: [f32; 3] = [251.0, 191.0, 36.0];
const WHITE: [f32; 3] = [230.0, 237.0, 247.0];

/// `size × size` RGBA (önceden çarpılmamış) logo pikselleri.
pub fn rgba(size: usize) -> Vec<u8> {
    let scale = VIEW / size as f32;
    let mut out = Vec::with_capacity(size * size * 4);
    for py in 0..size {
        for px in 0..size {
            // Piksel merkezi, SVG birimlerinde. Kenar yumuşatma genişliği bir piksel.
            let (x, y) = ((px as f32 + 0.5) * scale, (py as f32 + 0.5) * scale);
            let cover = |sd: f32| (0.5 - sd / scale).clamp(0.0, 1.0);

            let bg = cover(rounded_square(x, y, 56.0));
            let mut color = BACKGROUND;
            let mut paint = |c: [f32; 3], a: f32| {
                for i in 0..3 {
                    color[i] += (c[i] - color[i]) * a;
                }
            };
            paint(CYAN, cover(ring(x, y)));
            let tail = capsule(x, y, TAIL.0, TAIL.1);
            paint(BACKGROUND, cover(tail - STROKE / 2.0 - TAIL_OUTLINE));
            paint(AMBER, cover(tail - STROKE / 2.0));
            let sat = circle(x, y, SATELLITE);
            paint(BACKGROUND, cover(sat - SATELLITE_RADIUS - SATELLITE_OUTLINE / 2.0));
            paint(AMBER, cover(sat - SATELLITE_RADIUS + SATELLITE_OUTLINE / 2.0));
            paint(WHITE, cover(triangle(x, y) - PLAY_ROUNDING));

            out.extend(color.iter().map(|&c| c as u8));
            out.push((bg * 255.0) as u8);
        }
    }
    out
}

fn rounded_square(x: f32, y: f32, radius: f32) -> f32 {
    let half = VIEW / 2.0;
    let qx = (x - half).abs() - (half - radius);
    let qy = (y - half).abs() - (half - radius);
    (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - radius
}

fn circle(x: f32, y: f32, c: (f32, f32)) -> f32 {
    (x - c.0).hypot(y - c.1)
}

fn ring(x: f32, y: f32) -> f32 {
    (circle(x, y, RING_CENTER) - RING_RADIUS).abs() - STROKE / 2.0
}

/// Doğru parçasına uzaklık.
fn capsule(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (bax, bay) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * bax + (y - a.1) * bay) / (bax * bax + bay * bay)).clamp(0.0, 1.0);
    circle(x, y, (a.0 + t * bax, a.1 + t * bay))
}

/// Üçgene işaretli uzaklık (içeride negatif).
fn triangle(x: f32, y: f32) -> f32 {
    let mut dist = f32::INFINITY;
    let mut inside = true;
    for i in 0..3 {
        let (a, b) = (PLAY[i], PLAY[(i + 1) % 3]);
        dist = dist.min(capsule(x, y, a, b));
        // Köşeler saat yönünde (ekran koordinatları); içteki noktalar her kenarın aynı yanında.
        let cross = (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
        inside &= cross <= 0.0;
    }
    if inside { -dist } else { dist }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_transparent_and_center_is_opaque() {
        let size = 64;
        let px = rgba(size);
        assert_eq!(px.len(), size * size * 4);
        assert_eq!(px[3], 0, "sol üst köşe saydam olmalı");
        let center = (size / 2 * size + size / 2) * 4;
        assert_eq!(px[center + 3], 255);
    }

    /// Önizleme için: `QODA_LOGO_OUT=yol cargo test --bin qoda logo -- --ignored`
    #[test]
    #[ignore]
    fn export_preview() {
        if let Some(path) = std::env::var_os("QODA_LOGO_OUT") {
            std::fs::write(path, rgba(256)).unwrap();
        }
    }
}
