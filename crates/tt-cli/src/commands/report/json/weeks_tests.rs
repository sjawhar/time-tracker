//! Tests for the multi-week `--json --weeks N` report shape.

use chrono::{NaiveDate, TimeZone, Utc};
use insta::assert_snapshot;
use serde_json::Value;

use super::super::{Period, generate_report_data_for_date};
use super::{JsonWeeksReport, build_json_report};

fn build_weeks_json(reference_dates: &[NaiveDate]) -> String {
    let db = tt_db::Database::open_in_memory().unwrap();
    let generated_at = Utc.with_ymd_and_hms(2025, 2, 5, 12, 0, 0).unwrap();
    let reports = reference_dates
        .iter()
        .map(|date| {
            generate_report_data_for_date(
                &db,
                Period::Week,
                generated_at,
                *date,
                "Etc/UTC".to_string(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let weeks_report = JsonWeeksReport {
        weeks: reports.iter().map(build_json_report).collect(),
    };
    serde_json::to_string_pretty(&weeks_report).unwrap()
}

#[test]
fn test_weekly_reports_json_shape() {
    let reference_dates = vec![
        NaiveDate::from_ymd_opt(2025, 2, 5).unwrap(),
        NaiveDate::from_ymd_opt(2025, 1, 29).unwrap(),
    ];
    let output = build_weeks_json(&reference_dates);
    let json: Value = serde_json::from_str(&output).unwrap();
    let weeks = json
        .get("weeks")
        .and_then(|value| value.as_array())
        .unwrap();

    assert_eq!(json.as_object().unwrap().len(), 1);
    assert_eq!(weeks.len(), 2);
    let first_start = weeks[0]["period"]["start"].as_str().unwrap();
    let second_start = weeks[1]["period"]["start"].as_str().unwrap();
    assert!(first_start > second_start);

    assert_snapshot!(output, @r#"
    {
      "weeks": [
        {
          "generated_at": "2025-02-05T12:00:00+00:00",
          "timezone": "Etc/UTC",
          "week_start_day": "monday",
          "period": {
            "start": "2025-02-03",
            "end": "2025-02-09",
            "type": "week"
          },
          "by_tag": [],
          "streams": [],
          "untagged": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "streams": []
          },
          "agent_sessions": {
            "total": 0,
            "by_source": {},
            "by_type": {},
            "top_sessions": []
          },
          "totals": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "stream_count": 0,
            "unassigned_direct_ms": 0,
            "unassigned_delegated_ms": 0,
            "total_tracked_ms": 0
          }
        },
        {
          "generated_at": "2025-02-05T12:00:00+00:00",
          "timezone": "Etc/UTC",
          "week_start_day": "monday",
          "period": {
            "start": "2025-01-27",
            "end": "2025-02-02",
            "type": "week"
          },
          "by_tag": [],
          "streams": [],
          "untagged": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "streams": []
          },
          "agent_sessions": {
            "total": 0,
            "by_source": {},
            "by_type": {},
            "top_sessions": []
          },
          "totals": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "stream_count": 0,
            "unassigned_direct_ms": 0,
            "unassigned_delegated_ms": 0,
            "total_tracked_ms": 0
          }
        }
      ]
    }
    "#);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "inline snapshot of three full weekly reports"
)]
fn test_weekly_reports_ordering() {
    let reference_dates = vec![
        NaiveDate::from_ymd_opt(2025, 2, 5).unwrap(),
        NaiveDate::from_ymd_opt(2025, 1, 29).unwrap(),
        NaiveDate::from_ymd_opt(2025, 1, 22).unwrap(),
    ];
    let output = build_weeks_json(&reference_dates);
    let json: Value = serde_json::from_str(&output).unwrap();
    let weeks = json
        .get("weeks")
        .and_then(|value| value.as_array())
        .unwrap();

    assert_eq!(weeks.len(), 3);
    let first_start = weeks[0]["period"]["start"].as_str().unwrap();
    let second_start = weeks[1]["period"]["start"].as_str().unwrap();
    let third_start = weeks[2]["period"]["start"].as_str().unwrap();
    assert!(first_start > second_start);
    assert!(second_start > third_start);

    assert_snapshot!(output, @r#"
    {
      "weeks": [
        {
          "generated_at": "2025-02-05T12:00:00+00:00",
          "timezone": "Etc/UTC",
          "week_start_day": "monday",
          "period": {
            "start": "2025-02-03",
            "end": "2025-02-09",
            "type": "week"
          },
          "by_tag": [],
          "streams": [],
          "untagged": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "streams": []
          },
          "agent_sessions": {
            "total": 0,
            "by_source": {},
            "by_type": {},
            "top_sessions": []
          },
          "totals": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "stream_count": 0,
            "unassigned_direct_ms": 0,
            "unassigned_delegated_ms": 0,
            "total_tracked_ms": 0
          }
        },
        {
          "generated_at": "2025-02-05T12:00:00+00:00",
          "timezone": "Etc/UTC",
          "week_start_day": "monday",
          "period": {
            "start": "2025-01-27",
            "end": "2025-02-02",
            "type": "week"
          },
          "by_tag": [],
          "streams": [],
          "untagged": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "streams": []
          },
          "agent_sessions": {
            "total": 0,
            "by_source": {},
            "by_type": {},
            "top_sessions": []
          },
          "totals": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "stream_count": 0,
            "unassigned_direct_ms": 0,
            "unassigned_delegated_ms": 0,
            "total_tracked_ms": 0
          }
        },
        {
          "generated_at": "2025-02-05T12:00:00+00:00",
          "timezone": "Etc/UTC",
          "week_start_day": "monday",
          "period": {
            "start": "2025-01-20",
            "end": "2025-01-26",
            "type": "week"
          },
          "by_tag": [],
          "streams": [],
          "untagged": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "streams": []
          },
          "agent_sessions": {
            "total": 0,
            "by_source": {},
            "by_type": {},
            "top_sessions": []
          },
          "totals": {
            "time_direct_ms": 0,
            "time_delegated_ms": 0,
            "stream_count": 0,
            "unassigned_direct_ms": 0,
            "unassigned_delegated_ms": 0,
            "total_tracked_ms": 0
          }
        }
      ]
    }
    "#);
}
