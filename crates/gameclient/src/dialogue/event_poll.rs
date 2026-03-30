use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use serde::Deserialize;

use super::{DialogueState, NotificationData, NotificationQueue};

#[derive(Resource, Default)]
pub struct EventPollState {
    pub timer: Option<Timer>,
    pub known_active_ids: Vec<String>,
    pub fetched: Arc<Mutex<Option<Vec<ActiveEvent>>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActiveEvent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: serde_json::Value, // we'll parse the type field
    #[serde(default)]
    pub outcomes: Vec<serde_json::Value>,
}

pub fn poll_active_events(
    time: Res<Time>,
    mut poll: Local<EventPollState>,
    mut dialogue: ResMut<DialogueState>,
    mut notifications: ResMut<NotificationQueue>,
) {
    // Initialize timer
    if poll.timer.is_none() {
        let mut t = Timer::from_seconds(3.0, TimerMode::Repeating);
        t.tick(std::time::Duration::from_secs(3));
        poll.timer = Some(t);
    }

    // Tick timer separately to avoid borrow issues
    let just_finished = {
        let timer = poll.timer.as_mut().unwrap();
        timer.tick(time.delta());
        timer.just_finished()
    };

    // Check for fetched results — take data out to avoid borrow conflicts
    let fetched_events = poll.fetched.lock().ok().and_then(|mut lock| lock.take());

    if let Some(events) = fetched_events {
        {
            // Find newly active events (not in known list)
            for event in &events {
                if poll.known_active_ids.contains(&event.id) {
                    continue;
                }

                // New active event!
                let event_type = event.kind.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown");

                match event_type {
                    "npc_dialogue" | "random_encounter" => {
                        // Open dialogue box
                        if !dialogue.active {
                            let speaker = event.kind.get("speaker")
                                .and_then(|s| s.as_str())
                                .or_else(|| event.kind.get("enemy_name").and_then(|s| s.as_str()))
                                .unwrap_or(&event.name)
                                .to_string();

                            let lines: Vec<String> = event.kind.get("lines")
                                .and_then(|l| l.as_array())
                                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                                .or_else(|| {
                                    event.kind.get("description")
                                        .and_then(|d| d.as_str())
                                        .map(|d| vec![d.to_string()])
                                })
                                .unwrap_or_else(|| vec![event.description.clone()]);

                            dialogue.active = true;
                            dialogue.event_id = event.id.clone();
                            dialogue.speaker = speaker;
                            dialogue.lines = lines;
                            dialogue.current_line = 0;
                            dialogue.typewriter_index = 0;
                            dialogue.typewriter_timer = 0.0;
                        }
                    }
                    "story_beat" => {
                        // Show as notification
                        let lines: Vec<String> = event.kind.get("lines")
                            .and_then(|l| l.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .unwrap_or_default();

                        for line in lines {
                            notifications.pending.push(NotificationData {
                                text: line,
                                duration: 4.0,
                            });
                        }
                    }
                    "treasure" => {
                        // Show notification
                        let desc = event.kind.get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("Found treasure!");
                        notifications.pending.push(NotificationData {
                            text: desc.to_string(),
                            duration: 3.0,
                        });
                    }
                    "shop" => {
                        // Open dialogue-style shop display
                        if !dialogue.active {
                            let merchant = event.kind.get("merchant_name")
                                .and_then(|s| s.as_str())
                                .unwrap_or("Merchant")
                                .to_string();

                            let items: Vec<String> = event.kind.get("items")
                                .and_then(|i| i.as_array())
                                .map(|arr| arr.iter().map(|item| {
                                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                    let cost = item.get("cost").and_then(|c| c.as_i64()).unwrap_or(0);
                                    format!("  {} - {} gold", name, cost)
                                }).collect())
                                .unwrap_or_default();

                            let mut lines = vec!["Welcome to my shop!".to_string()];
                            lines.extend(items);
                            lines.push("(Shopping coming soon!)".to_string());

                            dialogue.active = true;
                            dialogue.event_id = event.id.clone();
                            dialogue.speaker = merchant;
                            dialogue.lines = lines;
                            dialogue.current_line = 0;
                            dialogue.typewriter_index = 0;
                            dialogue.typewriter_timer = 0.0;
                        }
                    }
                    _ => {
                        // Generic notification
                        notifications.pending.push(NotificationData {
                            text: format!("{}: {}", event.name, event.description),
                            duration: 3.0,
                        });
                    }
                }

                poll.known_active_ids.push(event.id.clone());
            }

        // Remove completed events from known list
        let active_ids: Vec<String> = events.iter().map(|e| e.id.clone()).collect();
        poll.known_active_ids.retain(|id| active_ids.contains(id));
        }
    }

    // Fire fetch on timer
    if just_finished {
        let fetched = poll.fetched.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            if let Ok(resp) = client.get("http://127.0.0.1:3001/events/active").send().await {
                if let Ok(events) = resp.json::<Vec<ActiveEvent>>().await {
                    if let Ok(mut lock) = fetched.lock() {
                        *lock = Some(events);
                    }
                }
            }
        });
    }

    // Also check outcomes from any notifications
    // (auto-completed events push notifications via their outcomes)
}
