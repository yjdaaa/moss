use std::time::Instant;

use laser_core::raster::{DitherAlgorithm, dither};

fn main() {
    let input = vec![vec![127.0_f32; 2000]; 2000];
    let started = Instant::now();
    let output = dither(&input, DitherAlgorithm::FloydSteinberg, 128.0);
    let elapsed = started.elapsed();
    assert_eq!(output.len(), 2000);
    println!("2000x2000 Floyd-Steinberg: {:.3}s", elapsed.as_secs_f64());
}
