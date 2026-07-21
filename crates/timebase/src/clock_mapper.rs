//! Bounded robust affine mapper from a device source clock to Edge Timebase.

use std::collections::VecDeque;

use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct ClockMapperConfig {
    pub max_samples: usize,
    pub residual_reject_ns: u64,
    pub jump_reset_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockModel {
    pub slope: f64,
    pub offset_ns: f64,
    pub residual_rms_ns: f64,
    pub revision: u64,
    pub sample_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockUpdate {
    Accepted,
    RejectedOutlier,
    ResetAfterJump,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClockError {
    #[error("clock mapper requires at least four bounded samples")]
    InvalidCapacity,
    #[error("clock model is not ready")]
    NotReady,
    #[error("mapped time is outside u64")]
    OutOfRange,
}

#[derive(Debug, Clone, Copy)]
struct Point {
    source_ns: u64,
    edge_ns: u64,
}

#[derive(Debug)]
pub struct ClockMapper {
    config: ClockMapperConfig,
    points: VecDeque<Point>,
    model: Option<ClockModel>,
    revision: u64,
    last_source_ns: Option<u64>,
}

impl ClockMapper {
    pub fn new(config: ClockMapperConfig) -> Result<Self, ClockError> {
        if config.max_samples < 4 {
            return Err(ClockError::InvalidCapacity);
        }
        Ok(Self {
            config,
            points: VecDeque::with_capacity(config.max_samples),
            model: None,
            revision: 1,
            last_source_ns: None,
        })
    }

    pub fn observe(&mut self, source_ns: u64, edge_receive_ns: u64) -> ClockUpdate {
        if self
            .last_source_ns
            .is_some_and(|previous| source_ns <= previous)
        {
            self.reset();
            self.push(source_ns, edge_receive_ns);
            return ClockUpdate::ResetAfterJump;
        }
        if let Some(model) = self.model {
            let predicted = model.slope.mul_add(source_ns as f64, model.offset_ns);
            let residual = (predicted - edge_receive_ns as f64).abs();
            if residual > self.config.jump_reset_ns as f64 {
                self.reset();
                self.push(source_ns, edge_receive_ns);
                return ClockUpdate::ResetAfterJump;
            }
            if self.points.len() >= 4 && residual > self.config.residual_reject_ns as f64 {
                self.last_source_ns = Some(source_ns);
                return ClockUpdate::RejectedOutlier;
            }
        }
        self.push(source_ns, edge_receive_ns);
        ClockUpdate::Accepted
    }

    #[must_use]
    pub const fn model(&self) -> Option<ClockModel> {
        self.model
    }

    pub fn map(&self, source_ns: u64) -> Result<u64, ClockError> {
        let model = self.model.ok_or(ClockError::NotReady)?;
        let mapped = model.slope.mul_add(source_ns as f64, model.offset_ns);
        if !mapped.is_finite() || mapped < 0.0 || mapped > u64::MAX as f64 {
            return Err(ClockError::OutOfRange);
        }
        Ok(mapped.round() as u64)
    }

    fn push(&mut self, source_ns: u64, edge_ns: u64) {
        if self.points.len() == self.config.max_samples {
            self.points.pop_front();
        }
        self.points.push_back(Point { source_ns, edge_ns });
        self.last_source_ns = Some(source_ns);
        self.model = fit(&self.points, self.revision);
    }

    fn reset(&mut self) {
        self.points.clear();
        self.model = None;
        self.last_source_ns = None;
        self.revision = self.revision.saturating_add(1);
    }
}

fn fit(points: &VecDeque<Point>, revision: u64) -> Option<ClockModel> {
    if points.len() < 2 {
        return None;
    }
    let origin_source = points.front()?.source_ns as f64;
    let origin_edge = points.front()?.edge_ns as f64;
    let count = points.len() as f64;
    let mean_x = points
        .iter()
        .map(|point| point.source_ns as f64 - origin_source)
        .sum::<f64>()
        / count;
    let mean_y = points
        .iter()
        .map(|point| point.edge_ns as f64 - origin_edge)
        .sum::<f64>()
        / count;
    let mut covariance = 0.0;
    let mut variance = 0.0;
    for point in points {
        let x = point.source_ns as f64 - origin_source - mean_x;
        let y = point.edge_ns as f64 - origin_edge - mean_y;
        covariance += x * y;
        variance += x * x;
    }
    if variance <= f64::EPSILON {
        return None;
    }
    let slope = covariance / variance;
    let offset_ns = (origin_edge + mean_y) - slope * (origin_source + mean_x);
    let residual_rms_ns = (points
        .iter()
        .map(|point| {
            let residual = slope.mul_add(point.source_ns as f64, offset_ns) - point.edge_ns as f64;
            residual * residual
        })
        .sum::<f64>()
        / count)
        .sqrt();
    Some(ClockModel {
        slope,
        offset_ns,
        residual_rms_ns,
        revision,
        sample_count: points.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::{ClockMapper, ClockMapperConfig, ClockUpdate};

    fn mapper() -> ClockMapper {
        ClockMapper::new(ClockMapperConfig {
            max_samples: 16,
            residual_reject_ns: 50_000,
            jump_reset_ns: 2_000_000,
        })
        .expect("mapper")
    }

    #[test]
    fn estimates_offset_and_drift() {
        let mut mapper = mapper();
        for index in 0..10_u64 {
            let source = index * 1_000_000;
            let edge = 5_000_000 + source + source / 10_000;
            assert_eq!(mapper.observe(source, edge), ClockUpdate::Accepted);
        }
        let mapped = mapper.map(20_000_000).expect("map");
        assert!(mapped.abs_diff(25_002_000) < 10);
        assert!(mapper.model().expect("model").residual_rms_ns < 1.0);
    }

    #[test]
    fn rejects_outlier_and_resets_on_clock_jump() {
        let mut mapper = mapper();
        for index in 0..5_u64 {
            mapper.observe(index * 1_000_000, 7_000_000 + index * 1_000_000);
        }
        assert_eq!(
            mapper.observe(6_000_000, 7_500_000),
            ClockUpdate::ResetAfterJump
        );
        assert_eq!(
            mapper.observe(5_000_000, 12_000_000),
            ClockUpdate::ResetAfterJump
        );
    }
}
