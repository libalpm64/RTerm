use super::*;
use std::sync::atomic::AtomicUsize;

#[tokio::test]
async fn outstanding_transfer_admission_is_bounded_and_released() {
    let context = eframe::egui::Context::default();
    let (backend, mut receiver) = super::super::Backend::new(&context);
    let events = backend.events.clone();
    let service = Service::default();
    let session = SessionService {
        session: None,
        transfer_slots: service.transfer_slots.clone(),
        outstanding_transfers: service.outstanding_transfers.clone(),
    };
    let second_session = SessionService {
        session: None,
        transfer_slots: service.transfer_slots.clone(),
        outstanding_transfers: service.outstanding_transfers.clone(),
    };
    let gate = Arc::new(Semaphore::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let drain = tokio::spawn(async move { while receiver.recv().await.is_some() {} });
    let enqueue = |session: &SessionService, i: usize| {
        let gate = gate.clone();
        let started = started.clone();
        session.transfer(
            &events,
            format!("fixture-{i}"),
            "fixture".into(),
            async move {
                started.fetch_add(1, Ordering::Relaxed);
                gate.acquire().await.unwrap().forget();
                Ok(TransferOutcome::Skipped)
            },
        )
    };
    for i in 0..MAX_OUTSTANDING_TRANSFERS {
        enqueue(
            if i % 2 == 0 {
                &session
            } else {
                &second_session
            },
            i,
        )
        .unwrap();
    }
    for i in MAX_OUTSTANDING_TRANSFERS..MAX_OUTSTANDING_TRANSFERS + 1000 {
        assert!(
            enqueue(&second_session, i)
                .unwrap_err()
                .contains("queue is full")
        );
    }
    assert_eq!(service.outstanding_transfers.available_permits(), 0);
    tokio::time::timeout(Duration::from_secs(5), async {
        while started.load(Ordering::Relaxed) < MAX_ACTIVE_TRANSFERS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(started.load(Ordering::Relaxed), MAX_ACTIVE_TRANSFERS);
    gate.add_permits(MAX_OUTSTANDING_TRANSFERS);
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.outstanding_transfers.available_permits() != MAX_OUTSTANDING_TRANSFERS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(started.load(Ordering::Relaxed), MAX_OUTSTANDING_TRANSFERS);
    assert_eq!(
        service.transfer_slots.available_permits(),
        MAX_ACTIVE_TRANSFERS
    );
    enqueue(&session, 10000).unwrap();
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.outstanding_transfers.available_permits() != MAX_OUTSTANDING_TRANSFERS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drain.abort();
}
