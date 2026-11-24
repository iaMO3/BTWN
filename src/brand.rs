fn round_distance(x: f32, y: f32, center: (f32, f32), half: (f32, f32), radius: f32) -> f32 {
    let dx = (x - center.0).abs() - half.0 + radius;
    let dy = (y - center.1).abs() - half.1 + radius;
    dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius
}

pub fn pixels(size: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let unit = 1.0 / size as f32;
    for y in 0..size {
        for x in 0..size {
            let x = (x as f32 + 0.5) * unit;
            let y = (y as f32 + 0.5) * unit;
            let coverage = |distance: f32| (0.5 - distance / unit).clamp(0.0, 1.0);
            let tile = coverage(round_distance(x, y, (0.5, 0.5), (0.46, 0.46), 0.21));
            let first = round_distance(x, y, (0.40, 0.43), (0.19, 0.146), 0.083);
            let second = round_distance(x, y, (0.60, 0.56), (0.19, 0.146), 0.083);
            let white = coverage(first.abs().min(second.abs()) - 0.026);
            for blue in [0.0, 113.0, 247.0] {
                rgba.push((blue + (255.0 - blue) * white).round() as u8);
            }
            rgba.push((255.0 * tile).round() as u8);
        }
    }
    rgba
}
