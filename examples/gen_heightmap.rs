//! Paints `assets/heightmap.png` (16-bit greyscale) and a coloured
//! `target/heightmap_preview.png` for eyeballing the layout.
//!
//! Run: `cargo run --example gen_heightmap`

#[path = "../src/config.rs"]
mod config;
#[path = "../src/geo.rs"]
mod geo;
#[path = "../src/heightgen.rs"]
mod heightgen;

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, entry) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *entry = c;
    }
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// zlib stream using uncompressed ("stored") deflate blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, c) in chunks.iter().enumerate() {
        out.push(if i + 1 == chunks.len() { 1 } else { 0 });
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// `color_type` 0 = greyscale, 2 = RGB. `raw` already has filter bytes.
fn png(res: usize, bit_depth: u8, color_type: u8, raw: &[u8]) -> Vec<u8> {
    png_wh(res, res, bit_depth, color_type, raw)
}

fn png_wh(w: usize, h: usize, bit_depth: u8, color_type: u8, raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_stored(raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn main() {
    let res = config::HEIGHTMAP_RES;
    let cell = config::WORLD_SIZE / (res - 1) as f32;
    let t0 = std::time::Instant::now();
    let heights = heightgen::generate();
    println!("generated in {:.1} s", t0.elapsed().as_secs_f32());
    let height = |x: f32, z: f32| {
        let ix = (((x + config::WORLD_HALF) / cell).round() as usize).min(res - 1);
        let iz = (((z + config::WORLD_HALF) / cell).round() as usize).min(res - 1);
        heights[iz * res + ix]
    };

    // Overlays for the preview: roads, bridges, bases.
    let mut overlay: Vec<Option<[u8; 3]>> = vec![None; res * res];
    let mut mark = |p: [f32; 2], c: [u8; 3], r: i32| {
        let ix = ((p[0] + config::WORLD_HALF) / cell).round() as i32;
        let iz = ((p[1] + config::WORLD_HALF) / cell).round() as i32;
        for dz in -r..=r {
            for dx in -r..=r {
                let (x, z) = (ix + dx, iz + dz);
                if x >= 0 && z >= 0 && (x as usize) < res && (z as usize) < res {
                    overlay[z as usize * res + x as usize] = Some(c);
                }
            }
        }
    };
    for r in 0..geo::ROADS.len() {
        for p in geo::road_polyline(r).iter().step_by(4) {
            mark(*p, [60, 60, 60], 0);
        }
    }
    let bridges = geo::find_bridges(height);
    for b in &bridges {
        for k in 0..=10 {
            let t = k as f32 / 10.0;
            mark([b.a[0] + (b.b[0] - b.a[0]) * t, b.a[1] + (b.b[1] - b.a[1]) * t], [255, 220, 0], 1);
        }
    }
    for base in [config::NATO_BASE, config::ENEMY_BASE] {
        mark(base.center, [255, 0, 0], 3);
    }

    let mut grey = Vec::with_capacity(res * (res * 2 + 1));
    let mut rgb = Vec::with_capacity(res * (res * 3 + 1));
    for (iz, row) in heights.chunks(res).enumerate() {
        grey.push(0); // filter: none
        rgb.push(0);
        for (ix, &h) in row.iter().enumerate() {
            grey.extend_from_slice(&heightgen::encode(h).to_be_bytes());
            let (x, z) = (-config::WORLD_HALF + ix as f32 * cell, -config::WORLD_HALF + iz as f32 * cell);
            let c: [u8; 3] = if let Some(c) = overlay[iz * res + ix] {
                c
            } else if h < 0.0 {
                [40, 80, 160]
            } else {
                let (lon, lat) = geo::lonlat(x, z);
                let t = (h / 900.0).clamp(0.0, 1.0);
                let mut c = [80.0 + 150.0 * t, 150.0 + 60.0 * t, 70.0 + 150.0 * t];
                let d = geo::desert(lon, lat);
                c = [c[0] + (220.0 - c[0]) * d, c[1] + (200.0 - c[1]) * d, c[2] + (140.0 - c[2]) * d];
                if geo::forest_density(x, z, h) > config::FOREST_THRESHOLD {
                    c = [c[0] * 0.55, c[1] * 0.7, c[2] * 0.55];
                }
                let s = geo::snow(lat, h);
                c = [c[0] + (240.0 - c[0]) * s, c[1] + (240.0 - c[1]) * s, c[2] + (245.0 - c[2]) * s];
                [c[0] as u8, c[1] as u8, c[2] as u8]
            };
            rgb.extend_from_slice(&c);
        }
    }

    std::fs::create_dir_all("assets").unwrap();
    std::fs::write(config::HEIGHTMAP_PATH, png(res, 16, 0, &grey)).unwrap();
    std::fs::create_dir_all("target").unwrap();
    std::fs::write("target/heightmap_preview.png", png(res, 8, 2, &rgb)).unwrap();
    // Close-up of the theatre (Europe), 2x.
    let [ex0, ez0] = geo::geo(-11.0, 66.0);
    let [ex1, ez1] = geo::geo(45.0, 35.0);
    let (ix0, iz0) = (((ex0 + config::WORLD_HALF) / cell) as usize, ((ez0 + config::WORLD_HALF) / cell) as usize);
    let (ix1, iz1) = (((ex1 + config::WORLD_HALF) / cell) as usize, ((ez1 + config::WORLD_HALF) / cell) as usize);
    let (w, hgt) = ((ix1 - ix0) * 2, (iz1 - iz0) * 2);
    let mut crop = Vec::with_capacity(hgt * (w * 3 + 1));
    for z in 0..hgt {
        crop.push(0);
        for x in 0..w {
            let src = (iz0 + z / 2) * (res * 3 + 1) + 1 + (ix0 + x / 2) * 3;
            crop.extend_from_slice(&rgb[src..src + 3]);
        }
    }
    std::fs::write("target/europe_preview.png", png_wh(w, hgt, 8, 2, &crop)).unwrap();

    let water = heights.iter().filter(|&&h| h < 0.0).count();
    let max = heights.iter().cloned().fold(f32::MIN, f32::max);
    println!(
        "wrote {} ({res}x{res}), {:.0}% water, max height {max:.0} m, {} bridges",
        config::HEIGHTMAP_PATH,
        100.0 * water as f32 / heights.len() as f32,
        bridges.len()
    );
    for b in &bridges {
        println!("  {} ({:.0} m, {})", geo::bridge_name(b), geo::dist(b.a, b.b), geo::ROADS[b.road].name);
    }
    for (name, base) in [("NATO", config::NATO_BASE), ("enemy", config::ENEMY_BASE)] {
        let c = base.center;
        let around: Vec<f32> = [(2_500.0, 0.0), (-2_500.0, 0.0), (0.0, 2_500.0), (0.0, -2_500.0)]
            .iter()
            .map(|(dx, dz)| height(c[0] + dx, c[1] + dz))
            .collect();
        println!("  {name} base at ({:.0}, {:.0}), terrain around: {around:?}", c[0], c[1]);
    }
}
