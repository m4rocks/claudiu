//! A spinner after shadcn's Spinner: Lucide's loader, turning.

use std::time::Duration;

use gpui::{Animation, AnimationElement, AnimationExt, ElementId};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use crate::panels::widgets::icon;

/// The "working" icon, `size` px square.
pub fn spinner(id: impl Into<ElementId>, size: f32) -> AnimationElement<Icon> {
    icon(IconName::LoaderCircle, size).with_animation(id, Animation::new(Duration::from_millis(1000)).repeat(), |i, t| i.rotate(gpui::radians(t * std::f32::consts::TAU)))
}
