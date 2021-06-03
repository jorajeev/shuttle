use shuttle::sync::Mutex;
use shuttle::thread;
use std::fmt;
use std::ops::Not;
use std::sync::Arc;
use tracing::info;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Location {
    Left,
    Right,
}

impl Not for Location {
    type Output = Self;
    fn not(self) -> Self::Output {
        use Location::*;
        match self {
            Left => Right,
            Right => Left,
        }
    }
}

struct State {
    farmer: Location,
    goat: Location,
    wolf: Location,
    hay: Location,
}

impl State {
    fn new() -> Self {
        use Location::*;
        Self {
            farmer: Left,
            goat: Left,
            wolf: Left,
            hay: Left,
        }
    }

    fn move_goat(&mut self) {
        if self.farmer == self.goat {
            self.farmer = !self.farmer;
            self.goat = !self.goat;
            info!("** moved goat {:?} **", self.goat);
        }
        self.check_goal();
    }

    fn move_wolf(&mut self) {
        if self.farmer == self.wolf && self.goat != self.hay {
            self.farmer = !self.farmer;
            self.wolf = !self.wolf;
            info!("** moved wolf {:?} **", self.wolf);
        }
        self.check_goal();
    }

    fn move_hay(&mut self) {
        if self.farmer == self.hay && self.goat != self.wolf {
            self.farmer = !self.farmer;
            self.hay = !self.hay;
            info!("** moved hay {:?} **", self.hay);
        }
        self.check_goal();
    }

    fn move_self(&mut self) {
        if self.goat != self.hay && self.goat != self.wolf {
            self.farmer = !self.farmer;
            info!("** moved self {:?} **", self.farmer);
        }
        self.check_goal();
    }

    fn check_goal(&self) {
        use Location::*;
        assert!(
            (self.farmer != Right) || (self.goat != Right) || (self.hay != Right) || (self.wolf != Right),
            "mission accomplished!"
        );
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("farmer", &self.farmer)
            .field("wolf", &self.wolf)
            .field("goat", &self.goat)
            .field("hay", &self.hay)
            .finish()
    }
}

macro_rules! action {
    ($label:literal, $act:stmt) => {
        thread::Builder::new().name($label.into()).spawn(move || loop {
            $act
            thread::yield_now();
        }).unwrap()
    }
}

fn farmer_puzzle() {
    let state = Arc::new(Mutex::new(State::new()));

    let state1 = state.clone();
    action!("move_self", {
        state1.lock().unwrap().move_self();
    });

    let state1 = state.clone();
    action!("move_wolf", {
        state1.lock().unwrap().move_wolf();
    });

    let state1 = state.clone();
    action!("move_goat", {
        state1.lock().unwrap().move_goat();
    });
    
    action!("move_hay", {
        state.lock().unwrap().move_hay();
    });
}

#[test]
fn farmer_puzzle_solve() {
    tracing_subscriber::fmt::init();
    shuttle::check_pct(farmer_puzzle,
        1_000,
        1,
    )
}
