use winisland_core::multitask::TaskFrame;
use winisland_render::{Path, PathBuilder, Point, Rect};

use super::activities::Activity;

const BRIDGE_DISTANCE: f32 = 7.5;

pub(crate) struct IslandSurface {
    pub(crate) primary: Path,
    pub(crate) outline: Path,
    pub(crate) secondary: Option<Rect>,
}

pub(crate) fn surface(
    island: Rect,
    radius: f32,
    scale: f32,
    compact_height: f32,
    left: bool,
    frame: Option<TaskFrame<Activity>>,
    notch: bool,
) -> IslandSurface {
    let primary = if notch {
        let ear_r = (8.0 * scale).min(radius).max(2.0);
        Path::notch(island, radius, ear_r)
    } else {
        Path::continuous_rounded_rect(island, radius)
    };
    let Some(frame) = frame else {
        return IslandSurface {
            outline: primary.clone(),
            primary,
            secondary: None,
        };
    };
    let separation = frame.separation.clamp(0.0, 1.0);
    let position = if frame.separation < 0.0 {
        -frame.separation * 3.0
    } else {
        frame.separation
    };
    let resting_height = compact_height.min(island.height()).max(0.0);
    let resting_width = frame.task.content.width(resting_height, scale);
    let shrink = 0.65 + separation * 0.35;
    let height = resting_height * shrink;
    let width = resting_width * shrink;
    let start = island.right - resting_width * 0.38;
    let end = island.right + resting_width / 2.0 + 8.0 * scale;
    let center_x = start + (end - start) * position;
    let center_y = island.center_y();
    let local_ball = Rect::from_xywh(
        center_x - width / 2.0,
        center_y - height / 2.0,
        width,
        height,
    );
    let ball = if left {
        Rect::from_xywh(
            island.left + island.right - local_ball.right,
            local_ball.top,
            width,
            height,
        )
    } else {
        local_ball
    };
    let gap = local_ball.left - island.right;
    let outline = if notch || gap >= BRIDGE_DISTANCE * scale {
        let mut path = PathBuilder::new();
        path.add_path(&primary);
        path.add_path(&Path::rounded_rect(ball, height / 2.0));
        path.detach()
    } else {
        joined_path(island, local_ball, radius, scale, left).unwrap_or_else(|| primary.clone())
    };
    IslandSurface {
        primary,
        outline,
        secondary: Some(ball),
    }
}

fn joined_path(island: Rect, ball: Rect, radius: f32, scale: f32, left: bool) -> Option<Path> {
    use std::f32::consts::{FRAC_PI_2, PI};

    let r1 = radius
        .min(island.height() / 2.0)
        .min(island.width() / 2.0)
        .max(scale);
    let r2 = ball.height() / 2.0;
    let c1 = Point::new(island.right - r1, island.center_y());
    let c2 = Point::new(ball.left + r2, ball.center_y());
    let gap = ball.left - island.right;
    let strength = (1.0 - gap.max(0.0) / (BRIDGE_DISTANCE * scale)).clamp(0.0, 1.0);
    let distance = c2.x - c1.x;
    if distance <= (r1 - r2).max(0.0) {
        return None;
    }
    let overlapping = distance < r1 + r2;
    let mut a1 = 60.0_f32.to_radians() * strength.sqrt();
    let mut a2 = 72.0_f32.to_radians() * strength.sqrt();
    let mut waist = r2 * 0.64 * strength.powf(1.25);
    if overlapping {
        let contact1 = ((distance * distance + r1 * r1 - r2 * r2) / (2.0 * distance * r1))
            .clamp(-1.0, 1.0)
            .acos();
        let contact2 = ((distance * distance + r2 * r2 - r1 * r1) / (2.0 * distance * r2))
            .clamp(-1.0, 1.0)
            .acos();
        let blend = ((distance - (r1 + r2 - r2 * 0.6)) / (r2 * 0.6)).clamp(0.0, 1.0);
        let blend = blend * blend * (3.0 - 2.0 * blend);
        a1 = contact1 + (a1.max(contact1) - contact1) * blend;
        a2 = contact2 + (a2.max(contact2) - contact2) * blend;
        waist = r1 * contact1.sin() * (1.0 - blend) + waist * blend;
    }
    let p2 = Point::new(c2.x - r2 * a2.cos(), c2.y - r2 * a2.sin());
    let flat_side = island.height() > r1 * 2.0 + 0.01;
    let p1 = if flat_side {
        Point::new(
            island.right,
            c1.y - (island.height() / 2.0 - r1).min(r2) * strength.sqrt(),
        )
    } else {
        Point::new(c1.x + r1 * a1.cos(), c1.y - r1 * a1.sin())
    };
    let mid = (p1.x + p2.x) * 0.5;
    let cy = (c1.y + c2.y) * 0.5;
    let handle = ((p2.x - p1.x).max(0.0) * 0.50).min(r2 * 1.4);
    let upper_tangent = if flat_side {
        Point::new(p1.x, p1.y + handle)
    } else {
        Point::new(p1.x + handle * a1.sin(), p1.y + handle * a1.cos())
    };
    let map = |p: Point| {
        if left {
            Point::new(island.left + island.right - p.x, p.y)
        } else {
            p
        }
    };
    let mut path = PathBuilder::new();
    path.move_to(map(Point::new(island.left + r1, island.top)));
    path.line_to(map(Point::new(island.right - r1, island.top)));
    if flat_side {
        arc(
            &mut path,
            Point::new(island.right - r1, island.top + r1),
            r1,
            -FRAC_PI_2,
            0.0,
            &map,
        );
        path.line_to(map(p1));
    } else {
        arc(&mut path, c1, r1, -FRAC_PI_2, -a1, &map);
    }
    path.cubic_to(
        map(upper_tangent),
        map(Point::new(mid - handle * 0.45, cy - waist)),
        map(Point::new(mid, cy - waist)),
    );
    path.cubic_to(
        map(Point::new(mid + handle * 0.45, cy - waist)),
        map(Point::new(
            p2.x - handle * a2.sin(),
            p2.y + handle * a2.cos(),
        )),
        map(p2),
    );
    arc(&mut path, c2, r2, -PI + a2, PI - a2, &map);
    path.cubic_to(
        map(Point::new(
            p2.x - handle * a2.sin(),
            2.0 * c2.y - p2.y - handle * a2.cos(),
        )),
        map(Point::new(mid + handle * 0.45, cy + waist)),
        map(Point::new(mid, cy + waist)),
    );
    path.cubic_to(
        map(Point::new(mid - handle * 0.45, cy + waist)),
        map(Point::new(upper_tangent.x, 2.0 * c1.y - upper_tangent.y)),
        map(Point::new(p1.x, 2.0 * c1.y - p1.y)),
    );
    if flat_side {
        path.line_to(map(Point::new(island.right, island.bottom - r1)));
        arc(
            &mut path,
            Point::new(island.right - r1, island.bottom - r1),
            r1,
            0.0,
            FRAC_PI_2,
            &map,
        );
    } else {
        arc(&mut path, c1, r1, a1, FRAC_PI_2, &map);
    }
    path.line_to(map(Point::new(island.left + r1, island.bottom)));
    arc(
        &mut path,
        Point::new(island.left + r1, island.bottom - r1),
        r1,
        FRAC_PI_2,
        PI,
        &map,
    );
    path.line_to(map(Point::new(island.left, island.top + r1)));
    arc(
        &mut path,
        Point::new(island.left + r1, island.top + r1),
        r1,
        PI,
        PI * 1.5,
        &map,
    );
    path.close();
    Some(path.detach())
}

fn arc(
    path: &mut PathBuilder,
    center: Point,
    radius: f32,
    start: f32,
    end: f32,
    map: &impl Fn(Point) -> Point,
) {
    let count = ((end - start).abs() / std::f32::consts::FRAC_PI_2)
        .ceil()
        .max(1.0) as usize;
    for index in 0..count {
        let from_angle = start + (end - start) * index as f32 / count as f32;
        let to_angle = start + (end - start) * (index + 1) as f32 / count as f32;
        let k = (4.0 / 3.0) * ((to_angle - from_angle) / 4.0).tan();
        let from = Point::new(
            center.x + radius * from_angle.cos(),
            center.y + radius * from_angle.sin(),
        );
        let to = Point::new(
            center.x + radius * to_angle.cos(),
            center.y + radius * to_angle.sin(),
        );
        path.cubic_to(
            map(Point::new(
                from.x - k * radius * from_angle.sin(),
                from.y + k * radius * from_angle.cos(),
            )),
            map(Point::new(
                to.x + k * radius * to_angle.sin(),
                to.y - k * radius * to_angle.cos(),
            )),
            map(to),
        );
    }
}
