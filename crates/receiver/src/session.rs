//! Session/episode lifecycle with frozen schema and bounded pre/post-roll.

use std::collections::VecDeque;

use robot_multicam_protocol::receiver::SynchronizedDatasetStep;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpisodeState {
    Recording,
    PostRoll,
    Finalized,
    Invalid,
}

#[derive(Debug, Clone)]
pub struct Episode {
    pub episode_id: Uuid,
    pub session_id: Uuid,
    pub manifest_revision: u64,
    pub state: EpisodeState,
    pub steps: Vec<SynchronizedDatasetStep>,
    post_roll_remaining: usize,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("session schema is frozen and cannot change while an episode is active")]
    SchemaFrozen,
    #[error("episode is already active or absent")]
    EpisodeState,
    #[error("step session, manifest revision, or cadence is invalid")]
    InvalidStep,
    #[error("episode step capacity exhausted")]
    Capacity,
}

#[derive(Debug)]
pub struct SessionManager {
    session_id: Uuid,
    manifest_revision: u64,
    schema_fingerprint: [u8; 32],
    pre_roll: VecDeque<SynchronizedDatasetStep>,
    pre_roll_capacity: usize,
    episode_capacity: usize,
    active: Option<Episode>,
}

impl SessionManager {
    pub fn new(
        session_id: Uuid,
        manifest_revision: u64,
        schema_fingerprint: [u8; 32],
        pre_roll_capacity: usize,
        episode_capacity: usize,
    ) -> Result<Self, SessionError> {
        if manifest_revision == 0 || pre_roll_capacity == 0 || episode_capacity == 0 {
            return Err(SessionError::Capacity);
        }
        Ok(Self {
            session_id,
            manifest_revision,
            schema_fingerprint,
            pre_roll: VecDeque::with_capacity(pre_roll_capacity),
            pre_roll_capacity,
            episode_capacity,
            active: None,
        })
    }

    pub fn update_schema(
        &mut self,
        manifest_revision: u64,
        schema_fingerprint: [u8; 32],
    ) -> Result<(), SessionError> {
        if self.active.is_some() {
            return Err(SessionError::SchemaFrozen);
        }
        if manifest_revision == 0 {
            return Err(SessionError::InvalidStep);
        }
        self.manifest_revision = manifest_revision;
        self.schema_fingerprint = schema_fingerprint;
        self.pre_roll.clear();
        Ok(())
    }

    pub fn ingest(&mut self, step: SynchronizedDatasetStep) -> Result<(), SessionError> {
        self.validate_step(&step)?;
        if let Some(episode) = &mut self.active {
            if episode.steps.len() == self.episode_capacity {
                episode.state = EpisodeState::Invalid;
                return Err(SessionError::Capacity);
            }
            episode.steps.push(step);
            if episode.state == EpisodeState::PostRoll {
                episode.post_roll_remaining = episode.post_roll_remaining.saturating_sub(1);
                if episode.post_roll_remaining == 0 {
                    episode.state = EpisodeState::Finalized;
                }
            }
        } else {
            if self.pre_roll.len() == self.pre_roll_capacity {
                self.pre_roll.pop_front();
            }
            self.pre_roll.push_back(step);
        }
        Ok(())
    }

    pub fn begin_episode(&mut self) -> Result<Uuid, SessionError> {
        if self.active.is_some() {
            return Err(SessionError::EpisodeState);
        }
        let episode_id = Uuid::new_v4();
        self.active = Some(Episode {
            episode_id,
            session_id: self.session_id,
            manifest_revision: self.manifest_revision,
            state: EpisodeState::Recording,
            steps: self.pre_roll.drain(..).collect(),
            post_roll_remaining: 0,
        });
        Ok(episode_id)
    }

    pub fn end_episode(&mut self, post_roll_steps: usize) -> Result<(), SessionError> {
        let episode = self.active.as_mut().ok_or(SessionError::EpisodeState)?;
        if episode.state != EpisodeState::Recording {
            return Err(SessionError::EpisodeState);
        }
        episode.post_roll_remaining = post_roll_steps;
        episode.state = if post_roll_steps == 0 {
            EpisodeState::Finalized
        } else {
            EpisodeState::PostRoll
        };
        Ok(())
    }

    pub fn take_finalized(&mut self) -> Result<Episode, SessionError> {
        if !self.active.as_ref().is_some_and(|episode| {
            matches!(
                episode.state,
                EpisodeState::Finalized | EpisodeState::Invalid
            )
        }) {
            return Err(SessionError::EpisodeState);
        }
        self.active.take().ok_or(SessionError::EpisodeState)
    }

    fn validate_step(&self, step: &SynchronizedDatasetStep) -> Result<(), SessionError> {
        if step.session_id != self.session_id.as_bytes()
            || step.manifest_revision != self.manifest_revision
            || step.capture_time_edge_ns == 0
        {
            return Err(SessionError::InvalidStep);
        }
        let last = self
            .active
            .as_ref()
            .and_then(|episode| episode.steps.last())
            .or_else(|| self.pre_roll.back());
        if last.is_some_and(|prior| step.capture_time_edge_ns <= prior.capture_time_edge_ns) {
            return Err(SessionError::InvalidStep);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{EpisodeState, SessionError, SessionManager};
    use robot_multicam_protocol::receiver::SynchronizedDatasetStep;
    use uuid::Uuid;

    #[test]
    fn schema_freezes_and_pre_post_roll_are_bounded() {
        let session = Uuid::new_v4();
        let mut manager = SessionManager::new(session, 1, [1; 32], 2, 5).expect("session");
        manager.ingest(step(session, 1, 1)).expect("step");
        manager.ingest(step(session, 1, 2)).expect("step");
        manager.ingest(step(session, 1, 3)).expect("step");
        manager.begin_episode().expect("begin");
        assert_eq!(
            manager.update_schema(2, [2; 32]),
            Err(SessionError::SchemaFrozen)
        );
        manager.ingest(step(session, 1, 4)).expect("step");
        manager.end_episode(1).expect("end");
        manager.ingest(step(session, 1, 5)).expect("post");
        let episode = manager.take_finalized().expect("finalized");
        assert_eq!(episode.state, EpisodeState::Finalized);
        assert_eq!(
            episode
                .steps
                .iter()
                .map(|step| step.capture_time_edge_ns)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
    }

    fn step(session: Uuid, revision: u64, time: u64) -> SynchronizedDatasetStep {
        SynchronizedDatasetStep {
            session_id: session.as_bytes().to_vec(),
            manifest_revision: revision,
            capture_time_edge_ns: time,
            valid: true,
            ..Default::default()
        }
    }
}
