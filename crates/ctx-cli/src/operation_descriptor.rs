pub(crate) use ctx_client_observability::operation_descriptor::*;

/// Explicit retained-CLI projection from protocol-owned tool identity to the
/// content-free product-operation fact owned by client observability.
pub(crate) const fn observed_mcp_product_operation(
    kind: ctx_agent_integrations::mcp::McpToolKind,
) -> Option<ObservedMcpProductOperation> {
    use ctx_agent_integrations::mcp::McpToolKind;

    match kind {
        McpToolKind::Status => Some(ObservedMcpProductOperation::Status),
        McpToolKind::Sources => Some(ObservedMcpProductOperation::Sources),
        McpToolKind::Search => Some(ObservedMcpProductOperation::Search),
        McpToolKind::ShowSession => Some(ObservedMcpProductOperation::ShowSession),
        McpToolKind::ShowEvent => Some(ObservedMcpProductOperation::ShowEvent),
        McpToolKind::QueryEvents => Some(ObservedMcpProductOperation::QueryEvents),
        McpToolKind::Blame => Some(ObservedMcpProductOperation::Blame),
        McpToolKind::Unknown | McpToolKind::Missing | McpToolKind::Unified(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use ctx_agent_integrations::mcp::McpToolKind;

    use super::*;

    #[test]
    fn protocol_identity_maps_once_to_observability_facts() {
        assert_eq!(
            observed_mcp_product_operation(McpToolKind::QueryEvents),
            Some(ObservedMcpProductOperation::QueryEvents)
        );
        assert_eq!(observed_mcp_product_operation(McpToolKind::Unknown), None);
        assert_eq!(observed_mcp_product_operation(McpToolKind::Missing), None);
    }
}
