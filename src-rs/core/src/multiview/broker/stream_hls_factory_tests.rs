//! Uses the real Factory/Unix FD handoff; no decoder or media subprocess.
use super::super::{
    resolvers::ServerSource,
    stream_hls::tests::{make_source, packets, playlist, Fixture, Reply},
};
use super::*;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn hls_probe_join_precedes_fresh_decode_and_final_cleanup() {
    let fixture = Fixture::start(vec![
        Reply::ok("/root", playlist(1, 1, true)),
        Reply::ok("/s1.ts", packets(7, 0x51)),
        Reply::ok("/root", playlist(2, 1, true)),
        Reply::ok("/s2.ts", packets(7, 0x52)),
    ])
    .await;
    let (source, _) = make_source(
        &fixture.base,
        ServerSource::hls_ts(&format!("{}/root", fixture.base), None).unwrap(),
    )
    .await;
    let mut factory = StreamFactory::new(source, Instant::now() + Duration::from_secs(5)).unwrap();
    assert!(matches!(
        factory.open(StreamPhase::Decode).await,
        Err(StreamError::InvalidPhase)
    ));
    for (phase, marker) in [(StreamPhase::Probe, 0x51), (StreamPhase::Decode, 0x52)] {
        let mut opened = factory.open(phase).await.unwrap();
        let mut reader = tokio::fs::File::from_std(opened.take_file().unwrap());
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bytes, packets(7, marker));
        drop(reader);
        let receipt = opened.retire().await.unwrap();
        assert!(receipt.joined && receipt.error.is_none());
    }
    let receipt = factory.finish().await.unwrap();
    assert!(receipt.producers_joined && receipt.provider_cleanup_confirmed);
    assert_eq!(factory.finish().await.unwrap(), receipt);
    fixture.finish().await;
}

#[tokio::test]
async fn actual_factory_finish_is_busy_while_hls_chunk_keeps_authorized_source() {
    let fixture = Fixture::start(vec![
        Reply::ok("/root", playlist(1, 1, true)),
        Reply::ok("/s1.ts", packets(7, 0x51)),
    ])
    .await;
    let (source, _) = make_source(
        &fixture.base,
        ServerSource::hls_ts(&format!("{}/root", fixture.base), None).unwrap(),
    )
    .await;
    let mut factory = StreamFactory::new(source, Instant::now() + Duration::from_secs(5)).unwrap();
    let source = factory.source.as_ref().unwrap().clone();
    let mut input = source
        .open_input(StreamLimits::new(tokio::time::Instant::from_std(
            factory.hard_end,
        )))
        .await
        .unwrap();
    let chunk = input.next_chunk().await.unwrap().unwrap();
    let held = chunk.slice(10..20);
    drop(chunk);
    drop(input);
    drop(source);
    assert_eq!(factory.finish().await, Err(StreamError::Busy));
    assert!(factory.source.is_some());
    assert!(factory.terminal.is_none());
    drop(held);
    let receipt = factory.finish().await.unwrap();
    assert!(receipt.producers_joined && receipt.provider_cleanup_confirmed);
    fixture.finish().await;
}
