//! Listener head model (`assets/la_dame_de_brassempouy_centered.glb`),
//! loaded the way `app.js` does with GLTFLoader: node translation applied,
//! `rotation.y = -π/2` (glTF forward → scene +X front), uniform scale so the
//! bounding box's largest dimension is 0.34 scene units, vertex colours kept.

use std::path::Path;

use glam::{Mat3, Vec3};

use super::MeshVertex;

pub const TARGET_MAX_DIMENSION: f32 = 0.34;

pub fn load(path: &Path) -> Result<(Vec<MeshVertex>, Vec<u32>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc = gltf::Gltf::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let blob = doc
        .blob
        .as_deref()
        .ok_or_else(|| format!("{}: no binary chunk", path.display()))?;

    let mut vertices: Vec<MeshVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for node in doc.nodes() {
        let Some(mesh) = node.mesh() else { continue };
        let (translation, rotation, scale) = node.transform().decomposed();
        let node_rot = glam::Quat::from_array(rotation);
        let node_tr = Vec3::from_array(translation);
        let node_scale = Vec3::from_array(scale);
        for prim in mesh.primitives() {
            let reader = prim.reader(|buffer| {
                (buffer.index() == 0 && matches!(buffer.source(), gltf::buffer::Source::Bin))
                    .then_some(blob)
            });
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|n| n.collect())
                .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; positions.len()]);
            let colors: Vec<[f32; 4]> = reader
                .read_colors(0)
                .map(|c| c.into_rgba_f32().collect())
                .unwrap_or_else(|| vec![[1.0; 4]; positions.len()]);
            let base = vertices.len() as u32;
            for i in 0..positions.len() {
                let p = node_rot * (Vec3::from_array(positions[i]) * node_scale) + node_tr;
                let n = node_rot * Vec3::from_array(*normals.get(i).unwrap_or(&[0.0, 1.0, 0.0]));
                vertices.push(MeshVertex {
                    pos: p.to_array(),
                    normal: n.to_array(),
                    color: *colors.get(i).unwrap_or(&[1.0; 4]),
                });
            }
            match reader.read_indices() {
                Some(idx) => indices.extend(idx.into_u32().map(|i| i + base)),
                None => indices.extend(base..base + positions.len() as u32),
            }
        }
    }
    if vertices.is_empty() {
        return Err(format!("{}: no geometry", path.display()));
    }

    // Bounding box → uniform scale to the target size, then rotation.y = -π/2.
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for v in &vertices {
        let p = Vec3::from_array(v.pos);
        min = min.min(p);
        max = max.max(p);
    }
    let extent = max - min;
    let largest = extent.x.max(extent.y).max(extent.z).max(1e-6);
    let scale = TARGET_MAX_DIMENSION / largest;
    let rot = Mat3::from_rotation_y(-std::f32::consts::FRAC_PI_2);
    for v in &mut vertices {
        v.pos = (rot * (Vec3::from_array(v.pos) * scale)).to_array();
        v.normal = (rot * Vec3::from_array(v.normal))
            .normalize_or_zero()
            .to_array();
        // Vertex colours are sRGB in glTF; the renderer shades in linear.
        v.color = [
            v.color[0].powf(2.2),
            v.color[1].powf(2.2),
            v.color[2].powf(2.2),
            v.color[3],
        ];
    }
    log::info!(
        "[head] {}: {} vertices, {} triangles, largest extent {:.1} → {TARGET_MAX_DIMENSION}",
        path.display(),
        vertices.len(),
        indices.len() / 3,
        largest
    );
    Ok((vertices, indices))
}
