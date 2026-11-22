//! Timeline geometry: lanes, zoom and pan, bar merging, equal mode, and
//! intervals mapped back to real time.

use desktop::trajectory::timeline::{
    DurationMode, Lane, TimelineLayout, Viewport,
};
use desktop::trajectory::{InFlight, TimeInterval};
use protocol::{
    AssistantRecord, ModelUsage, StopReason, TimestampMs, ToolCallId,
    ToolRecord, TrajectoryRecord, TrajectoryRecordKind,
};

/// A finished tool call over `start..end`.
fn tool(id: &str, start: u64, end: u64) -> TrajectoryRecord {
    TrajectoryRecord::builder()
        .id(id.to_string())
        .sequence(start)
        .started_at_ms(Some(TimestampMs::from(start)))
        .ended_at_ms(Some(TimestampMs::from(end)))
        .kind(TrajectoryRecordKind::Tool(
            ToolRecord::builder()
                .call_id(ToolCallId::try_from(id).expect("call"))
                .name("read".into())
                .arguments(serde_json::Value::Null)
                .result_text(Some("ok".into()))
                .build(),
        ))
        .build()
}

/// A model request sent at `sent`, first token at `first`, last at `last`.
fn request(id: &str, (sent, first, last): (u64, u64, u64)) -> TrajectoryRecord {
    TrajectoryRecord::builder()
        .id(id.to_string())
        .sequence(sent)
        .started_at_ms(Some(TimestampMs::from(sent)))
        .ended_at_ms(Some(TimestampMs::from(last)))
        .kind(TrajectoryRecordKind::Assistant(
            AssistantRecord::builder()
                .request_index(1)
                .provider_id("deepseek".into())
                .model_id("flash".into())
                .usage(ModelUsage::default())
                .requested_at_ms(TimestampMs::from(sent))
                .first_token_at_ms(TimestampMs::from(first))
                .last_token_at_ms(TimestampMs::from(last))
                .stop_reason(StopReason::EndTurn)
                .build(),
        ))
        .build()
}

/// Overlapping tools pack into three lanes.
#[test]
fn overlapping_tools_pack_into_three_lanes() {
    let records: Vec<_> =
        (0..5).map(|n| tool(&format!("t{n}"), 0, 100)).collect();
    let layout =
        TimelineLayout::new(&records, &[], 1_000, DurationMode::Actual);
    let lanes: Vec<(Lane, u32)> = layout
        .spans()
        .iter()
        .map(|span| (span.lane, span.merged))
        .collect();
    assert_eq!(
        lanes,
        [(Lane::Tool(0), 1), (Lane::Tool(1), 1), (Lane::Tool(2), 3)]
    );
}

/// Request span splits at the first token.
#[test]
fn request_span_splits_at_the_first_token() {
    let layout = TimelineLayout::new(
        &[request("a1", (1000, 1800, 2600))],
        &[],
        3_000,
        DurationMode::Actual,
    );
    let span = &layout.spans()[0];
    assert_eq!(span.lane, Lane::Request);
    assert_eq!(span.start, 1000.0);
    assert_eq!(span.first_token, Some(1800.0));
    assert_eq!(span.end, Some(2600.0));
}

/// Zoom keeps the anchor time.
#[test]
fn zoom_keeps_the_anchor_time() {
    let domain = 0.0..1000.0;
    let mut viewport = Viewport::full(&domain);
    let width = 400.0;
    let anchor_x = 100.0;
    assert_eq!(viewport.t_of(anchor_x, width), 250.0);
    viewport.zoom(2.0, 0.25, &domain);
    assert_eq!(viewport.t_of(anchor_x, width), 250.0);
    assert_eq!(viewport.visible.end - viewport.visible.start, 500.0);
    assert!(viewport.visible.start >= 0.0 && viewport.visible.end <= 1000.0);

    viewport.zoom(0.1, 0.5, &domain);
    assert_eq!(viewport.visible, domain, "zooming out stops at the domain");
}

/// Pan stops at the domain edges.
#[test]
fn pan_stops_at_the_domain_edges() {
    let domain = 0.0..1000.0;
    let mut viewport = Viewport::full(&domain);
    viewport.zoom(4.0, 0.5, &domain);
    viewport.pan(-10.0, &domain);
    assert_eq!(viewport.visible.start, 0.0);
    viewport.pan(-1.0, &domain);
    assert_eq!(viewport.visible.start, 0.0);
    viewport.pan(10.0, &domain);
    assert_eq!(viewport.visible.end, 1000.0);
}

/// Thin neighbours merge below a pixel.
#[test]
fn thin_neighbours_merge_below_a_pixel() {
    let records: Vec<_> = (0..1000)
        .map(|n| request(&format!("a{n}"), (n, n, n + 1)))
        .collect();
    let layout =
        TimelineLayout::new(&records, &[], 2_000, DurationMode::Actual);
    let bars = layout.bars(&Viewport::full(layout.domain()), 100.0);
    assert!(bars.len() <= 100, "{} bars", bars.len());
    assert_eq!(bars.iter().map(|bar| bar.merged).sum::<u32>(), 1000);
}

/// Equal mode is the default.
#[test]
fn equal_mode_is_the_default() {
    assert_eq!(DurationMode::default(), DurationMode::Equal);
}

/// Equal mode gives each record one unit.
#[test]
fn equal_mode_gives_each_record_one_unit() {
    let records = [
        request("a1", (0, 10, 500)),
        request("a2", (600, 610, 620)),
        request("a3", (700, 900, 5000)),
    ];
    let layout = TimelineLayout::new(&records, &[], 6_000, DurationMode::Equal);
    let units: Vec<(f64, Option<f64>)> = layout
        .spans()
        .iter()
        .map(|span| (span.start, span.end))
        .collect();
    assert_eq!(
        units,
        [(0.0, Some(1.0)), (1.0, Some(2.0)), (2.0, Some(3.0))]
    );
}

/// Interval maps back to real time in equal mode.
#[test]
fn interval_maps_back_to_real_time_in_equal_mode() {
    let records = [
        request("a1", (0, 10, 500)),
        request("a2", (600, 610, 620)),
        request("a3", (700, 900, 5000)),
    ];
    let layout = TimelineLayout::new(&records, &[], 6_000, DurationMode::Equal);
    let viewport = Viewport::full(layout.domain());
    // 300 px over 3 units: unit 2 spans 100..200 px.
    let interval = layout.interval(&viewport, 300.0, 120.0, 180.0);
    assert_eq!((interval.start_ms, interval.end_ms), (600, 620));

    assert_eq!(
        layout.hit(&viewport, 300.0, 150.0, Lane::Request),
        Some("a2")
    );
    // The interval covers the units of the records it overlaps.
    assert_eq!(layout.axis_range(&interval), Some(1.0..2.0));
}

/// A long request followed by a record that ended earlier: the highlight
/// covers the units of every record the interval overlaps, as the table's
/// filter keeps them, and never runs past them.
#[test]
fn the_highlight_covers_the_overlapping_units() {
    let records = [
        request("long", (0, 10, 100)),
        request("short", (10, 12, 20)),
        request("later", (30, 32, 40)),
    ];
    let layout = TimelineLayout::new(&records, &[], 200, DurationMode::Equal);
    let during = |start_ms, end_ms| TimeInterval { start_ms, end_ms };

    assert_eq!(layout.axis_range(&during(10, 20)), Some(0.0..2.0));
    assert_eq!(layout.axis_range(&during(50, 100)), Some(0.0..1.0));
    assert_eq!(layout.axis_range(&during(150, 160)), None);

    let actual = TimelineLayout::new(&records, &[], 200, DurationMode::Actual);
    assert_eq!(actual.axis_range(&during(10, 20)), Some(10.0..20.0));
}

/// Running span extends to now.
#[test]
fn running_span_extends_to_now() {
    let layout = TimelineLayout::new(
        &[],
        &[InFlight::Tool {
            call_id: "c1".into(),
            name: "read".into(),
            started_at_ms: 1_000,
        }],
        4_000,
        DurationMode::Actual,
    );
    let span = &layout.spans()[0];
    assert_eq!(span.end, Some(4000.0));
    assert!(span.running);
}

/// Lanes stack without overlap.
#[test]
fn lanes_stack_without_overlap() {
    let mut bottom = 0.0;
    for lane in Lane::ALL {
        let (top, height) = lane.band_rems();
        assert!(top >= bottom, "{lane:?} overlaps the lane above");
        assert_eq!(Lane::at_rems(top + height / 2.0), Some(lane));
        bottom = top + height;
    }
    assert!(bottom <= desktop::trajectory::timeline::TICKS_TOP_REMS);
    assert_eq!(Lane::at_rems(0.1), None, "run labels are not a lane");
}

/// A tool call logged before its result, as the history holds it while the
/// tool runs or after a run that never recorded the result.
fn pending_tool(call: &str, start: u64) -> TrajectoryRecord {
    let mut record = tool(call, start, start);
    record.id = format!("tool:{call}");
    record.ended_at_ms = None;
    if let TrajectoryRecordKind::Tool(tool) = &mut record.kind {
        tool.result_text = None;
    }
    record
}

/// A call both loaded and executing draws one bar, running to now.
#[test]
fn an_executing_tool_draws_one_bar() {
    let layout = TimelineLayout::new(
        &[pending_tool("c1", 1_000)],
        &[InFlight::Tool {
            call_id: "c1".into(),
            name: "read".into(),
            started_at_ms: 1_200,
        }],
        4_000,
        DurationMode::Actual,
    );
    assert_eq!(layout.spans().len(), 1);
    assert!(layout.spans()[0].running);
    assert_eq!(layout.spans()[0].end, Some(4000.0));
}

/// A call whose result never arrived is not running once nothing executes
/// it, so the overview stops growing.
#[test]
fn a_tool_without_result_runs_only_while_executing() {
    let layout = TimelineLayout::new(
        &[pending_tool("c1", 1_000)],
        &[],
        4_000,
        DurationMode::Actual,
    );
    assert!(!layout.running());
}
