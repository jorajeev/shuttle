use shuttle_engine::{Config, Runner};
use shuttle_schedulers::RoundRobinScheduler;

#[derive(Clone, Copy)]
struct Bits(u8);

#[test]
fn custom_causal_state_without_vector_clocks() {
    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );

    Runner::new(RoundRobinScheduler::new(1), config).run(|| {
        let (tx, rx) = shuttle_std::sync::mpsc::channel();
        let child = shuttle_std::thread::spawn(move || {
            shuttle_engine::current::causal::join_state(Bits(2));
            tx.send(()).unwrap();
        });

        rx.recv().unwrap();
        assert_eq!(shuttle_engine::current::causal::state::<Bits>().0, 3);
        child.join().unwrap();
    });
}
