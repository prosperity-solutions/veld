use serde::Serialize;

/// Progress events emitted by the orchestrator during `start()`.
///
/// The CLI decides how to render these — TTY gets live-updating lines,
/// non-TTY/JSON mode gets NDJSON for agent consumption.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressEvent {
    /// Execution plan resolved.
    PlanResolved { total_nodes: usize, stages: usize },

    /// A node is about to begin execution.
    NodeStarting {
        node: String,
        variant: String,
        index: usize,
        total: usize,
    },

    /// Port allocated for a server node.
    PortAllocated {
        node: String,
        variant: String,
        port: u16,
    },

    /// Health check phase started.
    HealthCheckPhase {
        node: String,
        variant: String,
        phase: u8,
        description: String,
    },

    /// Health check attempt (retry) within a phase.
    HealthCheckAttempt {
        node: String,
        variant: String,
        phase: u8,
        attempt: u32,
    },

    /// Health check phase passed.
    HealthCheckPassed {
        node: String,
        variant: String,
        phase: u8,
    },

    /// Node completed successfully.
    NodeHealthy {
        node: String,
        variant: String,
        url: Option<String>,
        elapsed_ms: u64,
    },

    /// Node was skipped (verify command passed).
    NodeSkipped { node: String, variant: String },

    /// Node failed.
    NodeFailed {
        node: String,
        variant: String,
        error: String,
    },

    /// Command step running.
    CommandRunning { node: String, variant: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_event_serialization() {
        let event = ProgressEvent::PlanResolved {
            total_nodes: 3,
            stages: 2,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"plan_resolved\""));
        assert!(json.contains("\"total_nodes\":3"));
        assert!(json.contains("\"stages\":2"));
    }

    #[test]
    fn test_node_starting_serialization() {
        let event = ProgressEvent::NodeStarting {
            node: "db".into(),
            variant: "docker".into(),
            index: 1,
            total: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"node_starting\""));
        assert!(json.contains("\"node\":\"db\""));
        assert!(json.contains("\"variant\":\"docker\""));
    }

    #[test]
    fn test_node_healthy_serialization() {
        let event = ProgressEvent::NodeHealthy {
            node: "api".into(),
            variant: "local".into(),
            url: Some("https://api.test.localhost".into()),
            elapsed_ms: 1234,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"node_healthy\""));
        assert!(json.contains("\"elapsed_ms\":1234"));
    }

    #[test]
    fn test_node_failed_serialization() {
        let event = ProgressEvent::NodeFailed {
            node: "redis".into(),
            variant: "docker".into(),
            error: "timeout".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"node_failed\""));
        assert!(json.contains("\"error\":\"timeout\""));
    }

    #[test]
    fn test_health_check_phase_serialization() {
        let event = ProgressEvent::HealthCheckPhase {
            node: "api".into(),
            variant: "local".into(),
            phase: 1,
            description: "waiting for port 8080".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"health_check_phase\""));
        assert!(json.contains("\"phase\":1"));
    }

    #[test]
    fn test_health_check_attempt_serialization() {
        let event = ProgressEvent::HealthCheckAttempt {
            node: "api".into(),
            variant: "local".into(),
            phase: 1,
            attempt: 5,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"health_check_attempt\""));
        assert!(json.contains("\"phase\":1"));
        assert!(json.contains("\"attempt\":5"));
    }
}
