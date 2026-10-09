use super::*;
const OWNER: ComponentId = ComponentId::from_raw(20);
const CONSUMER: ComponentId = ComponentId::from_raw(10);
const EP: EndpointId = EndpointId::from_raw(1);
const SERVER: TaskId = TaskId::from_raw(200);
const A: TaskId = TaskId::from_raw(100);
const B: TaskId = TaskId::from_raw(101);
fn setup() -> Exchange {
    let mut state = Exchange::new();
    state.listen(OWNER, SERVER, EP).unwrap();
    state.grant(OWNER, EP, CONSUMER).unwrap();
    state
}
#[test]
fn owned_copies_fifo_and_out_of_order_replies() {
    let mut s = setup();
    let mut bytes = [1, 2, 3];
    let a = s.submit(CONSUMER, A, EP, &bytes).unwrap().0;
    bytes.fill(9);
    let b = s.submit(CONSUMER, B, EP, &[4]).unwrap().0;
    let mut out = [0; 8];
    assert_eq!(s.receive(SERVER, EP, &mut out), Ok((a, CONSUMER, A, 3)));
    assert_eq!(&out[..3], &[1, 2, 3]);
    assert_eq!(s.receive(SERVER, EP, &mut out), Ok((b, CONSUMER, B, 1)));
    s.reply(SERVER, b, &[6]).unwrap();
    s.reply(SERVER, a, &bytes).unwrap();
    bytes.fill(7);
    assert_eq!(s.collect(B, b, &mut out), Ok((0, 1)));
    assert_eq!(out[0], 6);
    assert_eq!(s.collect(A, a, &mut out), Ok((0, 3)));
    assert_eq!(&out[..3], &[9, 9, 9]);
    assert_eq!(s.collect(A, a, &mut out), Err(Errno::ENOENT));
}
#[test]
fn permissions_bad_sizes_and_short_outputs_do_not_consume() {
    let mut s = setup();
    assert_eq!(s.submit(OWNER, A, EP, &[]), Err(Errno::EACCES));
    assert_eq!(
        s.submit(CONSUMER, A, EndpointId::from_raw(999), &[]),
        Err(Errno::ENOTCONN)
    );
    assert_eq!(
        s.submit(CONSUMER, A, EP, &[0; MESSAGE_MAX + 1]),
        Err(Errno::EMSGSIZE)
    );
    assert_eq!(s.grant(CONSUMER, EP, CONSUMER), Err(Errno::EACCES));
    let id = s.submit(CONSUMER, A, EP, &[1, 2]).unwrap().0;
    assert_eq!(s.receive(B, EP, &mut [0; 2]), Err(Errno::EACCES));
    assert_eq!(s.receive(SERVER, EP, &mut [0; 1]), Err(Errno::EMSGSIZE));
    s.receive(SERVER, EP, &mut [0; 2]).unwrap();
    assert_eq!(s.reply(B, id, &[]), Err(Errno::EACCES));
    assert_eq!(
        s.reply(SERVER, id, &[0; MESSAGE_MAX + 1]),
        Err(Errno::EMSGSIZE)
    );
    s.reply(SERVER, id, &[3, 4]).unwrap();
    assert_eq!(s.collect(B, id, &mut [0; 2]), Err(Errno::EACCES));
    assert_eq!(s.collect(A, id, &mut [0; 1]), Err(Errno::EMSGSIZE));
    assert_eq!(s.collect(A, id, &mut [0; 2]), Ok((0, 2)));
}
#[test]
fn bounded_inflight_includes_canceled_receipts_until_retirement() {
    let mut s = setup();
    let mut ids = Vec::new();
    for task in 0..4 {
        ids.push(
            s.submit(CONSUMER, TaskId::from_raw(task), EP, &[])
                .unwrap()
                .0,
        );
    }
    assert_eq!(s.submit(CONSUMER, A, EP, &[]), Err(Errno::ENOBUFS));
    let id = s.receive(SERVER, EP, &mut []).unwrap().0;
    s.cancel(TaskId::from_raw(0), id).unwrap();
    assert_eq!(
        s.collect(TaskId::from_raw(0), id, &mut []),
        Ok((Errno::ECANCELED.code(), 0))
    );
    assert_eq!(s.submit(CONSUMER, A, EP, &[]), Err(Errno::ENOBUFS));
    assert_eq!(s.reply(SERVER, id, b"late"), Err(Errno::ECANCELED));
    let replacement = s.submit(CONSUMER, A, EP, &[]).unwrap().0;
    assert!(replacement > ids[3]);
    assert_eq!(s.reply(SERVER, id, &[]), Err(Errno::ENOENT));
}
#[test]
fn first_terminal_wins_all_reply_cancel_close_orders() {
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut s = setup();
        let id = s.submit(CONSUMER, A, EP, &[]).unwrap().0;
        s.receive(SERVER, EP, &mut []).unwrap();
        assert_eq!(s.wait(A, EP, id), Ok(true));
        let mut wake_count = 0;
        for operation in order {
            wake_count += match operation {
                0 => s.reply(SERVER, id, &[]).ok().flatten().into_iter().count(),
                1 => s.cancel(A, id).ok().flatten().into_iter().count(),
                _ => s
                    .close(EP)
                    .into_iter()
                    .flatten()
                    .filter(|t| *t == A)
                    .count(),
            };
        }
        assert_eq!(wake_count, 1);
        let expected = [0, Errno::ECANCELED.code(), Errno::ENOTCONN.code()][order[0]];
        assert_eq!(s.collect(A, id, &mut []), Ok((expected, 0)));
        assert!(s.slots.iter().all(|slot| slot.id == 0));
    }
}
#[test]
fn caller_exit_and_server_exit_do_not_write_to_dead_callers() {
    let mut s = setup();
    let queued = s.submit(CONSUMER, A, EP, &[]).unwrap().0;
    s.exit(A);
    assert_eq!(s.collect(A, queued, &mut []), Err(Errno::ENOENT));
    let accepted = s.submit(CONSUMER, B, EP, &[]).unwrap().0;
    s.receive(SERVER, EP, &mut []).unwrap();
    s.exit(B);
    assert_eq!(s.collect(B, accepted, &mut []), Err(Errno::EACCES));
    assert_eq!(s.reply(SERVER, accepted, &[]), Err(Errno::ECANCELED));
    let id = s.submit(CONSUMER, A, EP, &[]).unwrap().0;
    assert_eq!(s.wait(A, EP, id), Ok(true));
    let (closed, wakes) = s.exit(SERVER);
    assert_eq!(closed.into_iter().flatten().collect::<Vec<_>>(), [EP]);
    assert_eq!(wakes.into_iter().flatten().collect::<Vec<_>>(), [A]);
    assert_eq!(s.collect(A, id, &mut []), Ok((Errno::ENOTCONN.code(), 0)));
}
#[test]
fn waiter_predicates_cover_early_reply_and_wake_before_park() {
    let mut s = setup();
    assert_eq!(s.wait(SERVER, EP, 0), Ok(true));
    let (id, wake) = s.submit(CONSUMER, A, EP, &[]).unwrap();
    assert_eq!(wake, Some(SERVER));
    assert_eq!(s.wait(SERVER, EP, 0), Ok(false));
    s.receive(SERVER, EP, &mut []).unwrap();
    assert_eq!(s.wait(A, EP, id), Ok(true));
    assert_eq!(s.reply(SERVER, id, &[]), Ok(Some(A)));
    assert_eq!(s.wait(A, EP, id), Ok(false));
    s.collect(A, id, &mut []).unwrap();
    let id = s.submit(CONSUMER, A, EP, &[]).unwrap().0;
    s.receive(SERVER, EP, &mut []).unwrap();
    assert_eq!(s.reply(SERVER, id, &[]), Ok(None));
    assert_eq!(s.wait(A, EP, id), Ok(false));
}
#[test]
fn cyclic_waits_are_rejected_and_new_endpoint_never_redirects_old_request() {
    let mut s = setup();
    let other = EndpointId::from_raw(2);
    s.listen(CONSUMER, A, other).unwrap();
    s.grant(CONSUMER, other, OWNER).unwrap();
    s.grant(OWNER, EP, OWNER).unwrap();
    assert_eq!(s.submit(OWNER, SERVER, EP, &[]), Err(Errno::EDEADLK));
    let old = s.submit(CONSUMER, A, EP, &[]).unwrap().0;
    assert_eq!(s.submit(OWNER, SERVER, other, &[]), Err(Errno::EDEADLK));
    s.close(EP);
    let new = EndpointId::from_raw(3);
    s.listen(OWNER, SERVER, new).unwrap();
    s.grant(OWNER, new, CONSUMER).unwrap();
    assert_eq!(s.collect(A, old, &mut []), Ok((Errno::ENOTCONN.code(), 0)));
    assert!(s.submit(CONSUMER, A, new, &[]).unwrap().0 > old);
}
