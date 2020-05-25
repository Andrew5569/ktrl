// std
use std::vec::Vec;
use std::collections::HashSet;

// ktrl
use crate::layers::LockOwner;
use crate::layers::LayersManager;
use crate::keys::KeyCode;
use crate::keys::KeyValue;
use crate::keys::KeyEvent;
use crate::effects::EffectValue;
use crate::effects::OutEffects;

// inner
use inner::inner;

use crate::layers::{
    Effect,
    Action,
    Action::TapHold,
};

const STOP: bool = true;
const CONTINUE: bool = false;
const TAP_HOLD_WAIT_PERIOD: i64 = 400000;

// This struct isn't used in Action::TapHold
// due to overhead it'll create in the config file.
// Lots of wrappers in the ron text
struct TapHoldCfg {
    tap_fx: Effect,
    hold_fx: Effect,
}

impl TapHoldCfg {
    fn from_action(action: &Action) -> Self {
        match action {
            TapHold(tap_fx, hold_fx) => Self{tap_fx: tap_fx.clone(),
                                             hold_fx: hold_fx.clone()},
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TapHoldWaiting {
    pub timestamp: evdev_rs::TimeVal,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TapHoldState {
    ThIdle,
    ThWaiting(TapHoldWaiting),
    ThHolding,
}

pub struct TapHoldMgr {
    // KEY_MAX elements
    states: Vec<TapHoldState>,

    // A list of keys that are currently in ThWaiting
    waiting_keys: Vec<KeyCode>,

    // A list of keys that are currently in ThHolding
    holding_keys: HashSet<KeyCode>,
}

// --------------- TapHold-specific Functions ----------------------

impl TapHoldMgr {
    pub fn new() -> Self {
        let mut states = Vec::new();
        states.resize_with(KeyCode::KEY_MAX as usize, || TapHoldState::ThIdle);

        Self{states,
             waiting_keys: Vec::new(),
             holding_keys: HashSet::new()}
    }

    fn lock_key(l_mgr: &mut LayersManager, key: KeyCode) {
        l_mgr.lock_key(key, LockOwner::LkTapHold);
    }

    fn unlock_key(l_mgr: &mut LayersManager, key: KeyCode) {
        l_mgr.unlock_key(key, LockOwner::LkTapHold);
    }

    fn insert_waiting(&mut self, l_mgr: &mut LayersManager, key: KeyCode) {
        self.waiting_keys.push(key);
        Self::lock_key(l_mgr, key);
    }

    fn clear_waiting(&mut self, l_mgr: &mut LayersManager) {
        for key in self.waiting_keys.drain(..) {
            Self::unlock_key(l_mgr, key);
        }
    }

    fn insert_holding(&mut self, l_mgr: &mut LayersManager, key: KeyCode) {
        self.holding_keys.insert(key);
        Self::lock_key(l_mgr, key);
    }

    fn remove_holding(&mut self, l_mgr: &mut LayersManager, key: KeyCode) {
        self.holding_keys.remove(&key);
        Self::unlock_key(l_mgr, key);
    }

    fn handle_th_holding(&mut self,
                         l_mgr: &mut LayersManager,
                         event: &KeyEvent,
                         th_cfg: &TapHoldCfg) -> OutEffects {
        let state = &mut self.states[event.code as usize];
        assert!(*state == TapHoldState::ThHolding);
        let value = KeyValue::from(event.value);

        match value {
            KeyValue::Press => {
                // Should never happen.
                // Should only see this in the idle state
                unreachable!()
            },

            KeyValue::Release => {
                // Cleanup the hold
                *state = TapHoldState::ThIdle;
                self.clear_waiting(l_mgr);
                self.remove_holding(l_mgr, event.code);
                OutEffects::new(STOP, th_cfg.hold_fx.clone(), KeyValue::Release) // forward the release
            },

            KeyValue::Repeat => {
                // Drop repeats. These aren't supported for TapHolds
                OutEffects::empty(STOP)
            }
        }
    }

    fn handle_th_waiting(&mut self,
                         l_mgr: &mut LayersManager,
                         event: &KeyEvent,
                         th_cfg: &TapHoldCfg) -> OutEffects {
        let state = &mut self.states[event.code as usize];
        let value = KeyValue::from(event.value);

        match value {
            KeyValue::Press => {
                // Should never happen.
                // Should only see this in the idle state
                unreachable!()
            },

            KeyValue::Release => {
                let is_wait_over = Self::is_waiting_over(state, event);
                *state = TapHoldState::ThIdle;
                self.clear_waiting(l_mgr);
                if is_wait_over {
                    OutEffects::new_multiple(STOP, vec![
                        EffectValue::new(th_cfg.hold_fx.clone(), KeyValue::Press),
                        EffectValue::new(th_cfg.hold_fx.clone(), KeyValue::Release)
                    ])
                } else {
                    OutEffects::new_multiple(STOP, vec![
                        EffectValue::new(th_cfg.tap_fx.clone(), KeyValue::Press),
                        EffectValue::new(th_cfg.tap_fx.clone(), KeyValue::Release)
                    ])
                }
            },

            KeyValue::Repeat => {
                // Drop repeats. These aren't supported for TapHolds
                OutEffects::empty(STOP)
            }
        }
    }

    fn handle_th_idle(&mut self,
                      l_mgr: &mut LayersManager,
                      event: &KeyEvent,
                      th_cfg: &TapHoldCfg) -> OutEffects {
        let state = &mut self.states[event.code as usize];
        assert!(*state == TapHoldState::ThIdle);

        let keycode: KeyCode = event.code;
        let value = KeyValue::from(event.value);

        match value {
            KeyValue::Press => {
                // Transition to the waiting state.
                // I.E waiting for either an interruptions => Press+Release the Tap effect
                // or for the TapHold wait period => Send a Hold effect press
                *state = TapHoldState::ThWaiting(
                    TapHoldWaiting{timestamp: event.time.clone()}
                );
                self.insert_waiting(l_mgr, keycode);
                OutEffects::empty(STOP)
            },

            KeyValue::Release => {
                // Forward the release
                OutEffects::new(STOP, th_cfg.tap_fx.clone(), KeyValue::Release)
            },

            KeyValue::Repeat => {
                // Drop repeats. These aren't supported for TapHolds
                OutEffects::empty(STOP)
            }
        }
    }

    // Assumes this is an event tied to a TapHold assigned MergedKey
    fn process_tap_hold_key(&mut self,
                            l_mgr: &mut LayersManager,
                            event: &KeyEvent,
                            th_cfg: &TapHoldCfg) -> OutEffects {
        match self.states[event.code as usize] {
            TapHoldState::ThIdle => self.handle_th_idle(l_mgr, event, th_cfg),
            TapHoldState::ThWaiting(_) => self.handle_th_waiting(l_mgr, event, th_cfg),
            TapHoldState::ThHolding => self.handle_th_holding(l_mgr, event, th_cfg),
        }
    }

    // --------------- Non-TapHold Functions ----------------------

    fn is_waiting_over(key_state: &TapHoldState, event: &KeyEvent) -> bool {
        let new_timestamp = event.time.clone();
        let wait_start_timestamp = inner!(key_state, if TapHoldState::ThWaiting).timestamp.clone();

        let secs_diff = new_timestamp.tv_sec - wait_start_timestamp.tv_sec;
        let usecs_diff  = new_timestamp.tv_usec - wait_start_timestamp.tv_usec;

        if secs_diff > 0 {
            true
        } else if usecs_diff > TAP_HOLD_WAIT_PERIOD {
            true
        } else {
            false
        }
    }

    fn process_non_tap_hold_key(&mut self,
                                l_mgr: &mut LayersManager,
                                event: &KeyEvent) -> OutEffects {
        let mut out = OutEffects::empty(CONTINUE);
        let waiting_keys: Vec<KeyCode> = self.waiting_keys.drain(..).collect();

        for waiting in waiting_keys {
            let action = &l_mgr.get(waiting).action;
            let th_cfg = TapHoldCfg::from_action(&action);

            Self::unlock_key(l_mgr, waiting);

            let state = &mut self.states[waiting as usize];
            if Self::is_waiting_over(&state, event) {
                // Append the press hold_fx to the output
                out.insert(th_cfg.hold_fx.clone(), KeyValue::Press);
                *state = TapHoldState::ThHolding;
                self.insert_holding(l_mgr, waiting);

            } else {
                // Flush the press and release tap_fx
                out.insert(th_cfg.tap_fx, KeyValue::Press);

                // Revert to the idle state
                *state = TapHoldState::ThIdle;
            }
        }

        out
    }

    // --------------- High-Level Functions ----------------------

    // Returns true if processed, false if skipped
    pub fn process(&mut self, l_mgr: &mut LayersManager, event: &KeyEvent) -> OutEffects {
        let code = event.code;
        let action = &l_mgr.get(code).action;

        if let Action::TapHold(..) = action {
            let th_cfg = TapHoldCfg::from_action(action);
            self.process_tap_hold_key(l_mgr, event, &th_cfg)
        } else {
            self.process_non_tap_hold_key(l_mgr, event)
        }
    }


    #[cfg(test)]
    pub fn is_idle(&self) -> bool {
        self.waiting_keys.len() == 0 &&
            self.holding_keys.len() == 0
    }
}

#[cfg(test)]
use crate::cfg::*;
#[cfg(test)]
use crate::keys::KeyCode::*;
#[cfg(test)]
use crate::effects::Effect::*;

#[test]
fn test_skipped() {
    let mut th_mgr = TapHoldMgr::new();
    let mut l_mgr = LayersManager::new(CfgLayers::empty());
    let ev_non_th_press = KeyEvent::new_press(KEY_A);
    let ev_non_th_release = KeyEvent::new_release(KEY_A);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_press), OutEffects::empty(CONTINUE));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_release), OutEffects::empty(CONTINUE));
    assert_eq!(th_mgr.is_idle(), true);
}

#[test]
fn test_tap() {
    let layers = CfgLayers::new(vec![
        // 0: base layer
        vec![
            (KEY_A, TapHold(Key(KEY_A), Key(KEY_LEFTCTRL))),
            (KEY_S, TapHold(Key(KEY_S), Key(KEY_LEFTALT))),
        ],
    ]);

    let mut l_mgr = LayersManager::new(layers);
    let mut th_mgr = TapHoldMgr::new();

    l_mgr.init();

    let ev_th_press = KeyEvent::new_press(KEY_A);
    let mut ev_th_release = KeyEvent::new_release(KEY_A);
    ev_th_release.time.tv_usec += 100;

    // 1st
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_press), OutEffects::empty(STOP));
    assert_eq!(th_mgr.is_idle(), false);
    assert_eq!(l_mgr.is_key_locked(KEY_A), true);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_release), OutEffects::new_multiple(STOP, vec![
        EffectValue::new(Effect::Key(KEY_A.into()), KeyValue::Press),
        EffectValue::new(Effect::Key(KEY_A.into()), KeyValue::Release),
    ]));
    assert_eq!(th_mgr.is_idle(), true);
    assert_eq!(l_mgr.is_key_locked(KEY_A), false);

    // 2nd
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_press), OutEffects::empty(STOP));
    assert_eq!(th_mgr.is_idle(), false);
    assert_eq!(l_mgr.is_key_locked(KEY_A), true);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_release), OutEffects::new_multiple(STOP, vec![
        EffectValue::new(Effect::Key(KEY_A.into()), KeyValue::Press),
        EffectValue::new(Effect::Key(KEY_A.into()), KeyValue::Release),
    ]));
    assert_eq!(th_mgr.is_idle(), true);
    assert_eq!(l_mgr.is_key_locked(KEY_A), false);

    // interruptions: 1
    ev_th_release.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 1;
    let ev_non_th_press = KeyEvent::new_press(KEY_W);
    let ev_non_th_release = KeyEvent::new_release(KEY_W);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_press), OutEffects::empty(STOP));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_press), OutEffects::new(CONTINUE, Effect::Key(KEY_A.into()), KeyValue::Press));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_release), OutEffects::new(STOP, Effect::Key(KEY_A.into()), KeyValue::Release));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_release), OutEffects::empty(CONTINUE));
    assert_eq!(th_mgr.is_idle(), true);

    // interruptions: 2
    ev_th_release.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 1;
    let ev_non_th_press = KeyEvent::new_press(KEY_W);
    let ev_non_th_release = KeyEvent::new_release(KEY_W);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_press), OutEffects::empty(STOP));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_press), OutEffects::new(CONTINUE, Effect::Key(KEY_A.into()), KeyValue::Press));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_release), OutEffects::empty(CONTINUE));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_release), OutEffects::new(STOP, Effect::Key(KEY_A.into()), KeyValue::Release));
    assert_eq!(th_mgr.is_idle(), true);
    assert_eq!(l_mgr.is_key_locked(KEY_A), false);
    assert_eq!(l_mgr.is_key_locked(KEY_W), false);
}

#[test]
fn test_hold() {
    let layers = CfgLayers::new(vec![
        // 0: base layer
        vec![
            (KEY_A, TapHold(Key(KEY_A), Key(KEY_LEFTCTRL))),
            (KEY_S, TapHold(Key(KEY_S), Key(KEY_LEFTALT))),
        ],
    ]);

    let mut l_mgr = LayersManager::new(layers);
    let mut th_mgr = TapHoldMgr::new();

    l_mgr.init();

    let ev_th_press = KeyEvent::new_press(KEY_A);
    let mut ev_th_release = KeyEvent::new_release(KEY_A);
    ev_th_release.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 1;

    // No hold + other key chord
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_press), OutEffects::empty(STOP));
    assert_eq!(l_mgr.is_key_locked(KEY_A), true);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_release), OutEffects::new_multiple(STOP, vec![
        EffectValue::new(Effect::Key(KEY_A.into()), KeyValue::Press),
        EffectValue::new(Effect::Key(KEY_A.into()), KeyValue::Release),
    ]));
    assert_eq!(th_mgr.is_idle(), true);
    assert_eq!(l_mgr.is_key_locked(KEY_A), false);


    // -------------------------------

    // Hold with other key
    let mut ev_non_th_press = KeyEvent::new_press(KEY_W);
    let mut ev_non_th_release = KeyEvent::new_release(KEY_W);
    ev_non_th_press.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 1;
    ev_non_th_release.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 2;
    ev_th_release.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 3;

    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_press), OutEffects::empty(STOP));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_press), OutEffects::new(CONTINUE, Effect::Key(KEY_LEFTCTRL.into()), KeyValue::Press));
    assert_eq!(th_mgr.is_idle(), false);
    assert_eq!(l_mgr.is_key_locked(KEY_A), true);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_release), OutEffects::empty(CONTINUE));
    assert_eq!(l_mgr.is_key_locked(KEY_A), true);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_release), OutEffects::new(STOP, Effect::Key(KEY_LEFTCTRL.into()), KeyValue::Release));
    assert_eq!(l_mgr.is_key_locked(KEY_A), false);

    // -------------------------------

    // Hold with other key (different order)
    ev_non_th_press.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 1;
    ev_th_release.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 2;
    ev_non_th_release.time.tv_usec = TAP_HOLD_WAIT_PERIOD + 3;

    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_press), OutEffects::empty(STOP));
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_press), OutEffects::new(CONTINUE, Effect::Key(KEY_LEFTCTRL.into()), KeyValue::Press));
    assert_eq!(th_mgr.is_idle(), false);
    assert_eq!(l_mgr.is_key_locked(KEY_A), true);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_th_release), OutEffects::new(STOP, Effect::Key(KEY_LEFTCTRL.into()), KeyValue::Release));
    assert_eq!(l_mgr.is_key_locked(KEY_A), false);
    assert_eq!(th_mgr.process(&mut l_mgr, &ev_non_th_release), OutEffects::empty(CONTINUE));
    assert_eq!(l_mgr.is_key_locked(KEY_A), false);
}
