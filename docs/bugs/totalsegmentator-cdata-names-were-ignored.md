# TotalSegmentator CDATA names were ignored

Fixed: 2026-09-26 10:57:46 CEST (+0200)

Commit before fix: `ccaa09eb4365218a36cbee9fa21699205b677974`

## Symptom

`inspect` reported `embedded_label_names: false` for TotalSegmentator `--ml` outputs, every palette entry had `name: null`, and anatomical labels received generic blocks instead of the bone, vessel, and organ families.

## Confirmed root cause

TotalSegmentator writes each name as `<Label Key="5" …><![CDATA[liver]]></Label>`. The label-table parser only collected `Event::Text`, so quick-xml's `Event::CData` was skipped. The test fixture used plain text, which is why the suite passed.

## Fix

The parser now accumulates plain text, CDATA, and entity references (`&amp;`, numeric character references) inside each `<Label>` and stores the trimmed result when the label closes. A TotalSegmentator-shaped fixture with CDATA names and an escaped name covers it in `tests/transform.rs`.
