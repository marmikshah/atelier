//! The humanoid rig: joint-space figure drawing (`figure`), the generated
//! side-view walk cycle (`walk`), and the named-gait moveset generator
//! (`pose_cycle`), plus the shared bone list / IK / scaffolding helpers that
//! flesh every pose identically.

use serde_json::{json, Value};

use super::Studio;
use atelier_core::raster;

impl Studio {
    /// Build a connected humanoid figure from named JOINT coordinates — the
    /// agent reasons in joint space (which it does well) instead of emitting
    /// every silhouette vertex (which it does not). Each bone is fleshed as an
    /// F1 capsule (`Document::stroke`) sharing its endpoints with its neighbours,
    /// so the whole figure is ONE connected, tapered silhouette by construction —
    /// no detached limbs, no blocky rect stacks. Re-pose by calling again with
    /// new joints. Required joints: head, shoulder_l/r, elbow_l/r, hand_l/r,
    /// hip_l/r, knee_l/r, foot_l/r (chest/pelvis are derived as the shoulder/hip
    /// midpoints).
    pub fn figure(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        joints: &std::collections::HashMap<String, (i32, i32)>,
        color: [u8; 4],
        limb_w: i32,
        torso_w: i32,
        head_r: i32,
        aa: bool,
        snap: bool,
    ) -> Result<Value, String> {
        let jf: std::collections::HashMap<String, (f32, f32)> = joints
            .iter()
            .map(|(k, &(x, y))| (k.clone(), (x as f32, y as f32)))
            .collect();
        let bones = humanoid_bones(&jf, limb_w.max(1), torso_w.max(1), head_r.max(1))?;
        let (dir, mut doc) = self.open(id)?;
        for b in &bones {
            doc.stroke_f(layer, frame, b, color, aa, false)?;
        }
        if snap {
            doc.snap_cel_to_own_palette(
                layer,
                frame,
                atelier_core::document::AlphaSnap::Opaque(128),
            );
        }
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "bones": bones.len()}))
    }

    /// Generate a side-view WALK CYCLE: from a base standing pose (the 13
    /// humanoid joints) plus gait parameters, compute each frame's joint table —
    /// feet stride along a gait path (one planted, one swinging, half a cycle
    /// apart), knees/elbows solved by 2-bone IK from the derived bone lengths,
    /// arms counter-swing the legs, the body bobs — then draw each frame with the
    /// connected-capsule figure and tag the range "walk". The walk is GENERATED
    /// from joints, not hand-repainted, so limbs never wobble or detach.
    pub fn walk(
        &self,
        id: &str,
        layer: usize,
        base: &std::collections::HashMap<String, (i32, i32)>,
        frames: usize,
        stride: i32,
        lift: i32,
        bob: i32,
        arm_swing: i32,
        color: [u8; 4],
        limb_w: i32,
        torso_w: i32,
        head_r: i32,
        aa: bool,
        snap: bool,
    ) -> Result<Value, String> {
        let g = |k: &str| joint_f(base, k);
        let frames = frames.clamp(2, 24);
        let (l_thigh, l_shin, l_uarm, l_farm) = rig_setup(base, limb_w, torso_w, head_r)?;
        let tau = std::f32::consts::TAU;
        let (dir, mut doc) = self.open(id)?;
        while doc.meta.frames.len() < frames {
            doc.add_frame(120, None);
        }
        // Per-frame: build the posed joint table, flesh it, draw into frame f.
        for f in 0..frames {
            let t = f as f32 / frames as f32;
            // Body bob: rises on the passing pose, twice per stride. Kept in f32
            // through to the sub-pixel stroke so the cycle glides, not steps.
            let body_dy = bob as f32 * (tau * t * 2.0).sin();
            let shift = |p: (f32, f32)| (p.0, p.1 + body_dy);
            let mut j: std::collections::HashMap<String, (f32, f32)> =
                std::collections::HashMap::new();
            // Body/girdle joints just bob.
            for k in ["head", "shoulder_l", "shoulder_r", "hip_l", "hip_r"] {
                j.insert(k.to_string(), shift(g(k)));
            }
            // Legs: foot strides front/back + lifts on the swing half; knee via IK.
            for (side, phase) in [("l", t), ("r", t + 0.5)] {
                let ph = phase.fract();
                let hip = shift(g(&format!("hip_{side}")));
                let base_foot = g(&format!("foot_{side}"));
                let fx = base_foot.0 + (stride as f32 * 0.5) * (tau * ph).cos();
                let fy = base_foot.1 - (lift as f32) * (tau * ph).sin().max(0.0);
                let foot = (fx, fy);
                let knee = ik_world(
                    hip, foot, l_thigh, l_shin,
                    true, // knee stays ahead of the hip (bends forward)
                );
                j.insert(format!("knee_{side}"), knee);
                j.insert(format!("foot_{side}"), foot);
            }
            // Arms counter-swing the legs (half-cycle offset); elbow via IK.
            for (side, phase) in [("l", t + 0.5), ("r", t)] {
                let ph = phase.fract();
                let sh = shift(g(&format!("shoulder_{side}")));
                let base_hand = g(&format!("hand_{side}"));
                let hx = base_hand.0 + (arm_swing as f32) * (tau * ph).cos();
                let hand = (hx, base_hand.1 + body_dy);
                let elbow = ik_world(
                    sh, hand, l_uarm, l_farm,
                    false, // elbow stays behind the shoulder (bends back)
                );
                j.insert(format!("elbow_{side}"), elbow);
                j.insert(format!("hand_{side}"), hand);
            }
            let bones = humanoid_bones(&j, limb_w.max(1), torso_w.max(1), head_r.max(1))?;
            doc.clear_cel(layer, f);
            for b in &bones {
                doc.stroke_f(layer, f, b, color, aa, false)?;
            }
            if snap {
                doc.snap_cel_to_own_palette(
                    layer,
                    f,
                    atelier_core::document::AlphaSnap::Opaque(128),
                );
            }
        }
        if !doc.meta.tags.iter().any(|t| t.name == "walk") {
            doc.add_tag("walk", 0, frames - 1, "forward")?;
        }
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "frames": frames, "tag": "walk"}))
    }

    /// Generate a full animation cycle for a named GAIT from one standing pose —
    /// the moveset generator. Same 13-joint contract and IK machinery as `walk`,
    /// with per-gait joint paths: `idle` (breathing bob), `run` (airborne
    /// stride, pumping arms, forward lean), `jump` (crouch → rise+tuck → fall →
    /// landing absorb), `attack` (lead-arm sweep with a lunge), `hurt` (recoil
    /// and recover). Amplitudes derive from the figure's own leg length scaled
    /// by `intensity`, so every preset fits any sprite size. Frames are tagged
    /// with the gait name; `frames=0` picks the gait's natural count.
    pub fn pose_cycle(
        &self,
        id: &str,
        layer: usize,
        base: &std::collections::HashMap<String, (i32, i32)>,
        gait: &str,
        frames: usize,
        intensity: f32,
        color: [u8; 4],
        limb_w: i32,
        torso_w: i32,
        head_r: i32,
        aa: bool,
        snap: bool,
    ) -> Result<Value, String> {
        const GAITS: &[(&str, usize)] = &[
            ("idle", 4),
            ("run", 6),
            ("jump", 8),
            ("attack", 4),
            ("hurt", 3),
        ];
        let Some(&(_, default_frames)) = GAITS.iter().find(|(g, _)| *g == gait) else {
            return Err(format!(
                "unknown gait '{gait}' — use one of [{}] (walk has its own tool)",
                GAITS.iter().map(|(g, _)| *g).collect::<Vec<_>>().join(", ")
            ));
        };
        let frames = if frames == 0 { default_frames } else { frames }.clamp(2, 24);
        let i = intensity.clamp(0.1, 3.0);
        let g = |k: &str| joint_f(base, k);
        let (l_thigh, l_shin, l_uarm, l_farm) = rig_setup(base, limb_w, torso_w, head_r)?;
        // The figure's own leg is the amplitude unit — presets scale with the sprite.
        let leg = l_thigh + l_shin;
        let tau = std::f32::consts::TAU;
        let pi = std::f32::consts::PI;
        let (dir, mut doc) = self.open(id)?;
        while doc.meta.frames.len() < frames {
            doc.add_frame(100, None);
        }
        for f in 0..frames {
            // Loop gaits sample the open interval (frame N would repeat frame 0);
            // one-shot gaits (jump/attack/hurt) sample the closed interval so the
            // last frame IS the recovery pose.
            let one_shot = matches!(gait, "jump" | "attack" | "hurt");
            let t = if one_shot {
                f as f32 / (frames - 1).max(1) as f32
            } else {
                f as f32 / frames as f32
            };
            // Per-gait offsets, all in leg-length units scaled by intensity.
            // (body_dx, body_dy): whole-figure shift. lean: extra upper-body x.
            // arm/foot overrides fill in below.
            // Per-side offset / position closures a gait may override.
            type SideOffset = Box<dyn Fn(&str) -> (f32, f32)>;
            type HandPos = Box<dyn Fn(&str, (f32, f32)) -> (f32, f32)>;
            let (body_dx, body_dy, lean): (f32, f32, f32);
            let mut foot_off: SideOffset = Box::new(|_| (0.0, 0.0));
            let mut hand_pos: Option<HandPos> = None;
            match gait {
                "idle" => {
                    body_dx = 0.0;
                    body_dy = 0.06 * leg * i * (tau * t).sin();
                    lean = 0.0;
                }
                "run" => {
                    body_dx = 0.0;
                    body_dy = 0.12 * leg * i * (tau * t * 2.0).sin();
                    lean = 0.15 * leg * i;
                    let (stride, lift) = (0.45 * leg * i, 0.35 * leg * i);
                    foot_off = Box::new(move |side: &str| {
                        let ph = (t + if side == "l" { 0.0 } else { 0.5 }).fract();
                        (stride * (tau * ph).cos(), -lift * (tau * ph).sin().max(0.0))
                    });
                    let swing = 0.5 * leg * i;
                    hand_pos = Some(Box::new(move |side: &str, base_hand: (f32, f32)| {
                        // Arms counter-swing the legs and pump upward mid-swing.
                        let ph = (t + if side == "l" { 0.5 } else { 0.0 }).fract();
                        (
                            base_hand.0 + swing * (tau * ph).cos(),
                            base_hand.1 - 0.15 * leg * (tau * ph).sin().abs(),
                        )
                    }));
                }
                "jump" => {
                    let (crouch, height, tuck) = (0.22 * leg * i, 0.55 * leg * i, 0.45 * leg * i);
                    // Piecewise: crouch → rise → fall → land, eased per phase.
                    let (dy, air_tuck) = if t < 0.3 {
                        let p = t / 0.3;
                        (crouch * (pi * p * 0.5).sin(), 0.0)
                    } else if t < 0.6 {
                        let p = (t - 0.3) / 0.3;
                        (-height * (pi * p * 0.5).sin(), tuck * p)
                    } else if t < 0.85 {
                        let p = (t - 0.6) / 0.25;
                        (-height * (1.0 - p * 0.85), tuck * (1.0 - p))
                    } else {
                        let p = (t - 0.85) / 0.15;
                        (crouch * 0.6 * (1.0 - p), 0.0)
                    };
                    body_dx = 0.0;
                    body_dy = dy;
                    lean = 0.0;
                    let airborne = (0.3..0.85).contains(&t);
                    foot_off = Box::new(move |_side: &str| {
                        if airborne {
                            // Feet ride with the body and tuck toward the hips.
                            (0.0, dy - air_tuck * 0.4)
                        } else {
                            (0.0, 0.0) // planted through crouch and landing
                        }
                    });
                    hand_pos = Some(Box::new(move |side: &str, base_hand: (f32, f32)| {
                        // Arms drive back in the crouch, throw up in the air.
                        let up = if airborne { -0.35 * leg } else { 0.12 * leg };
                        let back = if airborne { 0.0 } else { -0.15 * leg };
                        let _ = side;
                        (base_hand.0 + back, base_hand.1 + dy + up)
                    }));
                }
                "attack" => {
                    body_dx = 0.2 * leg * i * (pi * t).sin(); // lunge in, settle back
                    body_dy = 0.04 * leg * i * (pi * t).sin();
                    lean = 0.1 * leg * i * (pi * t).sin();
                    let reach = (l_uarm + l_farm) * 0.95;
                    hand_pos = Some(Box::new(move |side: &str, base_hand: (f32, f32)| {
                        if side == "r" {
                            // Lead hand sweeps an arc raised-behind → extended-front,
                            // SHOULDER-relative (resolved at the call site).
                            let a = (240.0 - 250.0 * t) * pi / 180.0;
                            let _ = base_hand;
                            (a.cos() * reach, a.sin() * reach)
                        } else {
                            // Guard hand pulls toward the chest.
                            (base_hand.0 - 0.1 * leg, base_hand.1 - 0.2 * leg)
                        }
                    }));
                }
                "hurt" => {
                    let r = 1.0 - t; // impact at t=0, recover by the end
                    body_dx = -0.25 * leg * i * r;
                    body_dy = 0.08 * leg * i * r;
                    lean = -0.2 * leg * i * r; // head/shoulders whip further back
                    hand_pos = Some(Box::new(move |_side: &str, base_hand: (f32, f32)| {
                        // Arms flail forward against the recoil.
                        (base_hand.0 + 0.3 * leg * r, base_hand.1 - 0.1 * leg * r)
                    }));
                }
                _ => unreachable!(),
            }
            let mut j: std::collections::HashMap<String, (f32, f32)> =
                std::collections::HashMap::new();
            for k in ["hip_l", "hip_r"] {
                let p = g(k);
                j.insert(k.to_string(), (p.0 + body_dx, p.1 + body_dy));
            }
            for k in ["head", "shoulder_l", "shoulder_r"] {
                let p = g(k);
                j.insert(k.to_string(), (p.0 + body_dx + lean, p.1 + body_dy));
            }
            // Legs: feet from the gait's offset (planted = base), knees by IK.
            for side in ["l", "r"] {
                let hip = j[&format!("hip_{side}")];
                let bf = g(&format!("foot_{side}"));
                let (fdx, fdy) = foot_off(side);
                let foot = (bf.0 + fdx, bf.1 + fdy);
                let knee = ik_world(hip, foot, l_thigh, l_shin, true);
                j.insert(format!("knee_{side}"), knee);
                j.insert(format!("foot_{side}"), foot);
            }
            // Arms: gait hand position (or hang with the body), elbows by IK.
            for side in ["l", "r"] {
                let sh = j[&format!("shoulder_{side}")];
                let bh = g(&format!("hand_{side}"));
                let hand = match &hand_pos {
                    Some(hp) if gait == "attack" && side == "r" => {
                        // Attack lead hand is shoulder-relative (an arc), not an offset.
                        let (ax, ay) = hp(side, bh);
                        (sh.0 + ax, sh.1 + ay)
                    }
                    Some(hp) => hp(side, bh),
                    None => (bh.0 + body_dx, bh.1 + body_dy),
                };
                let elbow = ik_world(sh, hand, l_uarm, l_farm, false);
                j.insert(format!("elbow_{side}"), elbow);
                j.insert(format!("hand_{side}"), hand);
            }
            let bones = humanoid_bones(&j, limb_w.max(1), torso_w.max(1), head_r.max(1))?;
            doc.clear_cel(layer, f);
            for b in &bones {
                doc.stroke_f(layer, f, b, color, aa, false)?;
            }
            if snap {
                doc.snap_cel_to_own_palette(
                    layer,
                    f,
                    atelier_core::document::AlphaSnap::Opaque(128),
                );
            }
        }
        if !doc.meta.tags.iter().any(|t| t.name == gait) {
            doc.add_tag(gait, 0, frames - 1, "forward")?;
        }
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "frames": frames, "tag": gait}))
    }
}

/// Joint lookup as f32 — the walk/pose_cycle pose tables are drawn sub-pixel.
fn joint_f(base: &std::collections::HashMap<String, (i32, i32)>, k: &str) -> (f32, f32) {
    let v = base[k];
    (v.0 as f32, v.1 as f32)
}

/// World-anchored IK: pick the bend that keeps the mid-joint on a consistent
/// world side (knees AHEAD of the hip, elbows BEHIND the shoulder) no matter
/// how the limb swings — otherwise solve_ik2's axis-relative bend flips the
/// joint to the wrong side mid-stride.
fn ik_world(root: (f32, f32), tgt: (f32, f32), l1: f32, l2: f32, ahead: bool) -> (f32, f32) {
    let c = raster::solve_ik2(root, tgt, l1, l2, 1.0);
    if (c.0 >= root.0) == ahead {
        c
    } else {
        raster::solve_ik2(root, tgt, l1, l2, -1.0)
    }
}

/// Shared walk/pose_cycle scaffolding: validate the joint contract, then read
/// the four bone lengths off the base pose (assumed left/right symmetric).
/// Returns (thigh, shin, upper-arm, forearm).
fn rig_setup(
    base: &std::collections::HashMap<String, (i32, i32)>,
    limb_w: i32,
    torso_w: i32,
    head_r: i32,
) -> Result<(f32, f32, f32, f32), String> {
    let base_f: std::collections::HashMap<String, (f32, f32)> = base
        .iter()
        .map(|(k, &(x, y))| (k.clone(), (x as f32, y as f32)))
        .collect();
    humanoid_bones(&base_f, limb_w.max(1), torso_w.max(1), head_r.max(1))?;
    let g = |k: &str| joint_f(base, k);
    let dist = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    Ok((
        dist(g("hip_l"), g("knee_l")).max(2.0),
        dist(g("knee_l"), g("foot_l")).max(2.0),
        dist(g("shoulder_l"), g("elbow_l")).max(2.0),
        dist(g("elbow_l"), g("hand_l")).max(2.0),
    ))
}

/// The humanoid capsule bone list for `figure`/`walk`: validates the 13 required
/// joints and returns each bone as a width-profiled point chain (drawn via the
/// `doc_stroke` core). Shared so a posed figure and an animated walk frame flesh
/// identically.
/// One bone as a width-profiled point chain `[(x,y,width), ...]` for the stroke core.
type Bone = Vec<(f32, f32, f32)>;

fn humanoid_bones(
    joints: &std::collections::HashMap<String, (f32, f32)>,
    lw: i32,
    tw: i32,
    hr: i32,
) -> Result<Vec<Bone>, String> {
    const NEED: [&str; 13] = [
        "head",
        "shoulder_l",
        "shoulder_r",
        "elbow_l",
        "elbow_r",
        "hand_l",
        "hand_r",
        "hip_l",
        "hip_r",
        "knee_l",
        "knee_r",
        "foot_l",
        "foot_r",
    ];
    for k in NEED {
        if !joints.contains_key(k) {
            return Err(format!(
                "missing joint '{k}' — required joints: {}",
                NEED.join(", ")
            ));
        }
    }
    let j = |k: &str| joints[k];
    let mid = |a: (f32, f32), b: (f32, f32)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
    let chest = mid(j("shoulder_l"), j("shoulder_r"));
    let pelvis = mid(j("hip_l"), j("hip_r"));
    let taper = |w: i32| (w * 7 / 10).max(1);
    let cap = |a: (f32, f32), w0: i32, b: (f32, f32), w1: i32| {
        vec![(a.0, a.1, w0 as f32), (b.0, b.1, w1 as f32)]
    };
    Ok(vec![
        cap(chest, tw, pelvis, (tw * 85 / 100).max(1)), // spine
        cap(j("shoulder_l"), lw, j("shoulder_r"), lw),  // clavicle
        cap(
            j("hip_l"),
            (lw * 11 / 10).max(1),
            j("hip_r"),
            (lw * 11 / 10).max(1),
        ), // hips
        cap(j("shoulder_l"), lw, j("elbow_l"), lw),     // upper arm L
        cap(j("elbow_l"), lw, j("hand_l"), taper(lw)),  // forearm L
        cap(j("shoulder_r"), lw, j("elbow_r"), lw),     // upper arm R
        cap(j("elbow_r"), lw, j("hand_r"), taper(lw)),  // forearm R
        cap(j("hip_l"), (lw * 12 / 10).max(1), j("knee_l"), lw), // thigh L
        cap(j("knee_l"), lw, j("foot_l"), taper(lw)),   // shin L
        cap(j("hip_r"), (lw * 12 / 10).max(1), j("knee_r"), lw), // thigh R
        cap(j("knee_r"), lw, j("foot_r"), taper(lw)),   // shin R
        cap(chest, lw, j("head"), lw),                  // neck
        vec![(j("head").0, j("head").1, (hr * 2) as f32)], // head disc
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-rig-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    fn standing_pose() -> std::collections::HashMap<String, (i32, i32)> {
        [
            ("head", (24, 9)),
            ("shoulder_l", (20, 15)),
            ("shoulder_r", (28, 15)),
            ("elbow_l", (18, 21)),
            ("elbow_r", (30, 21)),
            ("hand_l", (17, 27)),
            ("hand_r", (31, 27)),
            ("hip_l", (21, 27)),
            ("hip_r", (27, 27)),
            ("knee_l", (21, 35)),
            ("knee_r", (27, 35)),
            ("foot_l", (21, 43)),
            ("foot_r", (27, 43)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    #[test]
    fn pose_cycle_generates_every_gait_with_motion() {
        let s = studio("gaits");
        let pose = standing_pose();
        for gait in ["idle", "run", "jump", "attack", "hurt"] {
            let doc = format!("g-{gait}");
            s.doc_create(&doc, 48, 48).unwrap();
            let r = s
                .pose_cycle(
                    &doc,
                    0,
                    &pose,
                    gait,
                    0,
                    1.0,
                    [30, 30, 40, 255],
                    3,
                    5,
                    4,
                    true,
                    false,
                )
                .unwrap();
            assert_eq!(r["tag"], gait, "gait tag");
            let n = r["frames"].as_u64().unwrap() as usize;
            assert!(n >= 2);
            // Every frame drew something, and the cycle actually moves:
            // some frame differs from frame 0.
            let mut moved = false;
            for f in 1..n {
                let (_png, d) = s
                    .doc_frame_diff(&doc, 0, f, None, None, false, "none", None, 1)
                    .unwrap();
                if d["changed"].as_u64().unwrap_or(0) > 0 {
                    moved = true;
                }
            }
            assert!(moved, "{gait}: frames never changed — no motion generated");
        }
        // Unknown gait errors instead of guessing.
        s.doc_create("g-bad", 48, 48).unwrap();
        assert!(s
            .pose_cycle(
                "g-bad",
                0,
                &pose,
                "moonwalk",
                0,
                1.0,
                [0, 0, 0, 255],
                3,
                5,
                4,
                true,
                false
            )
            .is_err());
    }

    #[test]
    fn pose_cycle_jump_rises_above_standing() {
        let s = studio("jump");
        let pose = standing_pose();
        s.doc_create("j", 48, 48).unwrap();
        s.pose_cycle(
            "j",
            0,
            &pose,
            "jump",
            8,
            1.0,
            [30, 30, 40, 255],
            3,
            5,
            4,
            true,
            false,
        )
        .unwrap();
        // Silhouette top at mid-air (frame ~4) must be higher (smaller y)
        // than at frame 0 (crouch start), proving the body actually leaves.
        let top = |f: usize| {
            let r = s.doc_silhouette("j", f, None, 1).unwrap();
            r["bbox"][1].as_i64().unwrap()
        };
        assert!(
            top(4) < top(0) - 2,
            "airborne top {} should sit above standing top {}",
            top(4),
            top(0)
        );
    }
}
