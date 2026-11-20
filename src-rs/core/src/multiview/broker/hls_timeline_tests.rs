//! Pure timeline tests; fabricated opaque children test binding, not URI authority.
use super::*;

fn fixture(text: &str) -> Result<MediaPlaylist, TimelineError> {
    let base = Url::parse("https://source.invalid/live/index.m3u8?secret=synthetic").unwrap();
    let provenance = Arc::new(());
    let resources = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| HlsResource {
            url: base.join(line).unwrap(),
            kind: ResourceKind::Segment,
            provenance: Some(provenance.clone()),
        })
        .collect();
    parse_checked_parts(
        text.as_bytes(),
        &base,
        Some(&provenance),
        HlsManifest {
            resources,
            ended: text.lines().any(|line| line == "#EXT-X-ENDLIST"),
        },
    )
}
fn playlist(first: u64, names: &[&str], ended: bool) -> MediaPlaylist {
    let mut text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:{first}\n#EXT-X-DISCONTINUITY-SEQUENCE:3\n");
    for name in names {
        text.push_str(&format!("#EXTINF:6.000001,private title\n{name}\n"));
    }
    if ended {
        text.push_str("#EXT-X-ENDLIST\n");
    }
    fixture(&text).unwrap()
}
fn rejected(text: &str, error: TimelineError) {
    assert!(matches!(fixture(text), Err(actual) if actual == error));
}

#[test]
fn retains_exact_live_timeline_and_opaque_authorized_resource_order() {
    let p = playlist(42, &["one.ts?token=a", "two.ts?token=b"], false);
    assert_eq!(p.media_sequence, 42);
    assert_eq!(p.discontinuity_sequence, 3);
    assert_eq!(p.target_duration, Duration::from_secs(6));
    assert!(!p.end_list);
    assert_eq!(
        p.segments.iter().map(|s| s.sequence).collect::<Vec<_>>(),
        [42, 43]
    );
    assert_eq!(p.segments[0].duration, Duration::new(6, 1000));
    assert_eq!(p.segments[1].resource.url.query(), Some("token=b"));
    assert!(Arc::ptr_eq(
        p.segments[0].resource.provenance.as_ref().unwrap(),
        p.segments[1].resource.provenance.as_ref().unwrap()
    ));
}

#[test]
fn accepts_sequence_defaults_exact_none_key_crlf_and_finished_media() {
    let p = fixture("#EXTM3U\r\n#EXT-X-VERSION:3\r\n#EXT-X-TARGETDURATION:1\r\n#EXT-X-KEY:METHOD=NONE\r\n#EXT-X-INDEPENDENT-SEGMENTS\r\n#EXTINF:0.999999999,\r\nfirst.ts\r\n#EXT-X-ENDLIST\r\n").unwrap();
    assert_eq!(p.media_sequence, 0);
    assert_eq!(p.discontinuity_sequence, 0);
    assert_eq!(p.segments[0].duration, Duration::new(0, 999_999_999));
    assert!(p.end_list);
}

#[test]
fn rejects_unsupported_master_encryption_fmp4_ranges_low_latency_gaps_and_unknowns() {
    for tag in [
        "#EXT-X-STREAM-INF:BANDWIDTH=100",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"secret\"",
        "#EXT-X-KEY:METHOD=NONE,URI=\"secret\"",
        "#EXT-X-MAP:URI=\"init.mp4\"",
        "#EXT-X-BYTERANGE:10@0",
        "#EXT-X-PART:DURATION=1,URI=\"a.ts\"",
        "#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES",
        "#EXT-X-GAP",
        "#EXT-X-DISCONTINUITY",
        "#EXT-X-DEFINE:NAME=\"x\",VALUE=\"secret\"",
        "#EXT-X-UNREVIEWED:1",
    ] {
        rejected(
            &format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n{tag}\n#EXTINF:6,\none.ts\n"),
            TimelineError::Unsupported,
        );
    }
    for file in [
        "init.mp4",
        "audio.aac",
        "part.m4s",
        "text.vtt",
        "{$variable}.ts",
    ] {
        rejected(
            &format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n{file}\n"),
            TimelineError::Unsupported,
        );
    }
}

#[test]
fn rejects_missing_duplicate_malformed_duration_and_sequence_metadata() {
    for text in ["#EXTM3U\n#EXTINF:6,\na.ts\n", "#EXTM3U\n#EXT-X-TARGETDURATION:6\na.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n", "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6\na.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-MEDIA-SEQUENCE:1\n#EXTINF:6,\na.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n#EXT-X-MEDIA-SEQUENCE:1\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.5,\na.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0000000001,\na.ts\n"] {
        rejected(text, TimelineError::Invalid);
    }
    for value in ["NaN", "-1", "1e2", "1."] {
        rejected(
            &format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:{value},\na.ts\n"),
            TimelineError::Invalid,
        );
    }
}

#[test]
fn rejects_overflow_zero_excessive_duration_segment_count_and_body_size() {
    for value in ["0", "31"] {
        rejected(
            &format!("#EXTM3U\n#EXT-X-TARGETDURATION:{value}\n#EXTINF:1,\na.ts\n"),
            TimelineError::Unsupported,
        );
    }
    for value in ["0", "30.000000001"] {
        rejected(
            &format!("#EXTM3U\n#EXT-X-TARGETDURATION:30\n#EXTINF:{value},\na.ts\n"),
            TimelineError::Unsupported,
        );
    }
    rejected("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n#EXTINF:6,\na.ts\n", TimelineError::Overflow);
    rejected(
        "#EXTM3U\n#EXT-X-TARGETDURATION:18446744073709551616\n#EXTINF:6,\na.ts\n",
        TimelineError::Overflow,
    );
    let mut many = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n".to_owned();
    for n in 0..65 {
        many.push_str(&format!("#EXTINF:6,\n{n}.ts\n"));
    }
    rejected(&many, TimelineError::TooLarge);
    rejected(
        &format!("#EXTM3U\n#{}", "x".repeat(MAX_BYTES)),
        TimelineError::TooLarge,
    );
}

#[test]
fn refuses_mismatched_provenance_child_order_or_endlist_without_exposing_uri() {
    let base = Url::parse("https://source.invalid/index.m3u8").unwrap();
    let own = Arc::new(());
    for variant in 0..5 {
        let child = HlsResource {
            url: base
                .join(if variant == 2 {
                    "other.ts?secret=private"
                } else {
                    "a.ts"
                })
                .unwrap(),
            kind: if variant == 3 {
                ResourceKind::Map
            } else {
                ResourceKind::Segment
            },
            provenance: if variant == 1 {
                Some(Arc::new(()))
            } else {
                Some(own.clone())
            },
        };
        let result = parse_checked_parts(
            b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n",
            &base,
            if variant == 0 { None } else { Some(&own) },
            HlsManifest {
                resources: vec![child],
                ended: variant == 4,
            },
        );
        assert!(result.is_err());
        let reason = result.err().unwrap();
        assert!(!format!("{reason:?}").contains("private"));
        assert_eq!(
            reason,
            match variant {
                3 => TimelineError::Unsupported,
                4 => TimelineError::Invalid,
                _ => TimelineError::Provenance,
            }
        );
    }
}

#[test]
fn rfc_rounded_extinf_accepts_fractional_target_but_not_oversized_rounding() {
    let p = fixture("#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.033,\na.ts\n").unwrap();
    assert_eq!(p.segments[0].duration, Duration::from_millis(4033));
    rejected(
        "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.500000000,\na.ts\n",
        TimelineError::Invalid,
    );
}

#[test]
fn duplicate_uri_is_unsupported_only_inside_one_window_not_across_future_sequences() {
    rejected(
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n#EXTINF:6,\na.ts\n",
        TimelineError::Unsupported,
    );
    let old = playlist(10, &["a.ts"], false);
    let next = playlist(11, &["a.ts"], false);
    assert_eq!(old.segments[0].resource.url, next.segments[0].resource.url);
    assert_ne!(old.segments[0].sequence, next.segments[0].sequence);
}

#[test]
fn exact_segment_and_byte_bounds_are_inclusive_and_resources_cannot_be_reordered() {
    let names = (0..64).map(|n| format!("{n}.ts")).collect::<Vec<_>>();
    assert_eq!(
        playlist(
            1,
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
            false
        )
        .segments
        .len(),
        64
    );
    let mut text = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n".to_owned();
    while text.len() < MAX_BYTES {
        let remaining = MAX_BYTES - text.len();
        if remaining == 1 {
            text.push('\n');
        } else {
            let size = remaining.min(8000);
            text.push('#');
            text.push_str(&"x".repeat(size - 2));
            text.push('\n');
        }
    }
    assert_eq!(text.len(), MAX_BYTES);
    assert!(fixture(&text).is_ok());
    let base = Url::parse("https://source.invalid/list.m3u8").unwrap();
    let own = Arc::new(());
    let resources = ["b.ts", "a.ts"]
        .map(|name| HlsResource {
            url: base.join(name).unwrap(),
            kind: ResourceKind::Segment,
            provenance: Some(own.clone()),
        })
        .into_iter()
        .collect();
    let result = parse_checked_parts(
        b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n#EXTINF:6,\nb.ts\n",
        &base,
        Some(&own),
        HlsManifest {
            resources,
            ended: false,
        },
    );
    assert!(matches!(result, Err(TimelineError::Provenance)));
    assert!(matches!(
        parse_checked_parts(
            &[0xff],
            &base,
            Some(&own),
            HlsManifest {
                resources: vec![],
                ended: false
            }
        ),
        Err(TimelineError::Invalid)
    ));
}

fn master_fixture(
    text: &str,
    own: &Arc<()>,
    pinned: Option<&HlsResource>,
) -> Result<SelectedVariant, TimelineError> {
    let base = Url::parse("https://source.invalid/live/master.m3u8?token=secret").unwrap();
    let resources = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| HlsResource {
            url: base.join(line).unwrap(),
            kind: ResourceKind::Playlist,
            provenance: Some(own.clone()),
        })
        .collect();
    master_checked_parts(
        text.as_bytes(),
        &base,
        Some(own),
        HlsManifest {
            resources,
            ended: false,
        },
        pinned,
    )
}
#[test]
fn master_selects_one_self_contained_lowest_bandwidth_with_stable_tie_and_no_query_copy() {
    let own = Arc::new(());
    let selected = master_fixture("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-STREAM-INF:BANDWIDTH=2000,AVERAGE-BANDWIDTH=1500,RESOLUTION=1280x720,FRAME-RATE=25.000,CODECS=\"avc1.4d401f,mp4a.40.2\",CLOSED-CAPTIONS=NONE\nhigh.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nz-low.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=1000\na-low.m3u8\n", &own, None).unwrap();
    assert_eq!(selected.bandwidth, 1000);
    assert_eq!(selected.resource.url.path(), "/live/a-low.m3u8");
    assert_eq!(
        selected.resource.url.query(),
        None,
        "never copy master query credentials"
    );
    assert!(Arc::ptr_eq(
        selected.resource.provenance.as_ref().unwrap(),
        &own
    ));
}
#[test]
fn master_pin_survives_reorder_and_new_lower_variant_but_rejects_removed_or_foreign_pin() {
    let own = Arc::new(());
    let original = master_fixture(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000\nold.m3u8\n",
        &own,
        None,
    )
    .unwrap();
    let changed = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nnew.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2500\nold.m3u8\n";
    let pinned = master_fixture(changed, &own, Some(&original.resource)).unwrap();
    assert_eq!(pinned.resource.url, original.resource.url);
    assert_eq!(
        pinned.bandwidth, 2500,
        "return current parsed variant, not stale metadata"
    );
    assert!(matches!(
        master_fixture(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nnew.m3u8\n",
            &own,
            Some(&original.resource)
        ),
        Err(TimelineError::Provenance)
    ));
    assert!(matches!(
        master_fixture(changed, &Arc::new(()), Some(&original.resource)),
        Err(TimelineError::Provenance)
    ));
}
#[test]
fn master_rejects_alternate_renditions_unknown_features_cross_origin_and_nested_media_confusion() {
    let own = Arc::new(());
    for attr in [
        "AUDIO=\"external\"",
        "VIDEO=\"other\"",
        "SUBTITLES=\"subs\"",
        "CLOSED-CAPTIONS=\"group\"",
        "URI=\"extra.m3u8\"",
        "UNKNOWN=1",
    ] {
        assert!(matches!(
            master_fixture(
                &format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000,{attr}\na.m3u8\n"),
                &own,
                None
            ),
            Err(TimelineError::Unsupported)
        ));
    }
    for tag in [
        "#EXT-X-MEDIA:TYPE=AUDIO,URI=\"audio.m3u8\"",
        "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key\"",
        "#EXT-X-MAP:URI=\"init.mp4\"",
        "#EXTINF:6,",
    ] {
        assert!(matches!(
            master_fixture(
                &format!("#EXTM3U\n{tag}\n#EXT-X-STREAM-INF:BANDWIDTH=1000\na.m3u8\n"),
                &own,
                None
            ),
            Err(TimelineError::Unsupported)
        ));
    }
    assert!(matches!(
        master_fixture(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nhttps://other.invalid/a.m3u8\n",
            &own,
            None
        ),
        Err(TimelineError::Provenance)
    ));
    rejected(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nchild.m3u8\n",
        TimelineError::Unsupported,
    );
}
#[test]
fn master_enforces_variant_count_complete_binding_and_bounded_scalar_attributes() {
    let own = Arc::new(());
    let mut text = "#EXTM3U\n".to_owned();
    for n in 0..8 {
        text.push_str(&format!(
            "#EXT-X-STREAM-INF:BANDWIDTH={}\n{n}.m3u8\n",
            1000 + n
        ));
    }
    assert!(master_fixture(&text, &own, None).is_ok());
    text.push_str("#EXT-X-STREAM-INF:BANDWIDTH=2000\nninth.m3u8\n");
    assert!(matches!(
        master_fixture(&text, &own, None),
        Err(TimelineError::TooLarge)
    ));
    for attrs in [
        "BANDWIDTH=0",
        "BANDWIDTH=100000001",
        "BANDWIDTH=1,AUDIO=\"x\"",
        "BANDWIDTH=1,CODECS=\"https://secret\"",
    ] {
        assert!(matches!(
            master_fixture(
                &format!("#EXTM3U\n#EXT-X-STREAM-INF:{attrs}\na.m3u8\n"),
                &own,
                None
            ),
            Err(TimelineError::Unsupported)
        ));
    }
    for attrs in [
        "BANDWIDTH=1,BANDWIDTH=2",
        "BANDWIDTH=NaN",
        "BANDWIDTH=1,",
        "CODECS=\"avc1.4d401f\"",
        "BANDWIDTH=1,CODECS=\"unterminated",
    ] {
        assert!(matches!(
            master_fixture(
                &format!("#EXTM3U\n#EXT-X-STREAM-INF:{attrs}\na.m3u8\n"),
                &own,
                None
            ),
            Err(TimelineError::Invalid)
        ));
    }
    assert!(matches!(
        master_fixture("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\n", &own, None),
        Err(TimelineError::Invalid)
    ));
}
