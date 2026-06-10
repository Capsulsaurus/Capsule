//! Re-identification & pseudo-labeling primitives (SSoT: [AI/ML — Re-ID & Pseudo-Labeling]).
//!
//! Identifies individuals even when they turn away from the camera: a high-confidence frontal
//! face is matched to a profile, its box is linked to an overlapping body box (IoU > 0.7), and
//! other body crops in the same event are pseudo-labeled by cosine similarity to that body
//! embedding above a threshold. Pure geometry + vector math — detection/embedding come from a
//! runner.
//!
//! [AI/ML — Re-ID & Pseudo-Labeling]: https://docs/design/ai/#re-id--pseudo-labeling

use crate::ml::runner::BBox;

/// The default IoU above which a face box and a body box are taken to be the same person.
pub const DEFAULT_LINK_IOU: f32 = 0.7;

/// Intersection-over-union of two normalized boxes (`0.0` if they do not overlap).
pub fn iou(a: BBox, b: BBox) -> f32 {
    let ax2 = a.x + a.w;
    let ay2 = a.y + a.h;
    let bx2 = b.x + b.w;
    let by2 = b.y + b.h;
    let iw = (ax2.min(bx2) - a.x.max(b.x)).max(0.0);
    let ih = (ay2.min(by2) - a.y.max(b.y)).max(0.0);
    let inter = iw * ih;
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Cosine similarity of two equal-length vectors (`0.0` if either is zero or lengths differ).
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Whether a face box and a body box overlap enough to be linked as the same person
/// (`iou > iou_threshold`). Pass [`DEFAULT_LINK_IOU`] for the design's 0.7.
pub fn links_to_profile(face: BBox, body: BBox, iou_threshold: f32) -> bool {
    iou(face, body) > iou_threshold
}

/// Whether a candidate body crop should inherit a profile's pseudo-label: its embedding's cosine
/// similarity to the linked body embedding is at or above `sim_threshold`.
pub fn pseudo_labels(anchor_body: &[f32], candidate_body: &[f32], sim_threshold: f32) -> bool {
    cosine_sim(anchor_body, candidate_body) >= sim_threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bbox(x: f32, y: f32, w: f32, h: f32) -> BBox {
        BBox { x, y, w, h }
    }

    #[test]
    fn iou_of_identical_boxes_is_one_and_disjoint_is_zero() {
        let a = bbox(0.0, 0.0, 0.5, 0.5);
        assert!((iou(a, a) - 1.0).abs() < 1e-6);
        let b = bbox(0.6, 0.6, 0.2, 0.2);
        assert_eq!(iou(a, b), 0.0);
    }

    #[test]
    fn high_overlap_links_to_profile_low_does_not() {
        let face = bbox(0.10, 0.10, 0.40, 0.40);
        let body = bbox(0.11, 0.11, 0.40, 0.40); // ~94% IoU → linked
        assert!(links_to_profile(face, body, DEFAULT_LINK_IOU));
        let far = bbox(0.50, 0.50, 0.40, 0.40); // small overlap → not linked
        assert!(!links_to_profile(face, far, DEFAULT_LINK_IOU));
    }

    #[test]
    fn pseudo_label_above_threshold_only() {
        let anchor = vec![1.0f32, 0.0, 0.0];
        let same_dir = vec![2.0f32, 0.0, 0.0]; // cosine 1.0
        let orthogonal = vec![0.0f32, 1.0, 0.0]; // cosine 0.0
        assert!(pseudo_labels(&anchor, &same_dir, 0.8));
        assert!(!pseudo_labels(&anchor, &orthogonal, 0.8));
    }

    #[test]
    fn cosine_sim_handles_degenerate_inputs() {
        assert_eq!(cosine_sim(&[1.0, 2.0], &[1.0]), 0.0); // length mismatch
        assert_eq!(cosine_sim(&[0.0, 0.0], &[1.0, 1.0]), 0.0); // zero vector
    }
}
