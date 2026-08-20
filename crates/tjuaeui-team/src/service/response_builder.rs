use super::*;

impl TeamSessionService {
    pub(super) async fn build_team_response(&self, team: &Team) -> Result<TeamResponse, TeamError> {
        let mut agents = Vec::with_capacity(team.agents.len());
        for agent in &team.agents {
            agents.push(self.build_agent_response(agent).await?);
        }

        Ok(TeamResponse {
            id: team.id.clone(),
            name: team.name.clone(),
            workspace: team.workspace.clone(),
            assistants: agents,
            leader_assistant_id: team.lead_agent_id.clone(),
            created_at: team.created_at,
            updated_at: team.updated_at,
        })
    }

    pub(super) async fn build_agent_response(
        &self,
        agent: &TeamAgent,
    ) -> Result<tjuaeui_api_types::TeamAgentResponse, TeamError> {
        let icon = self.resolve_agent_icon(agent).await?;
        let mut response = agent.to_response_with_icon(icon);
        response.pending_confirmations = self.pending_confirmation_count(&agent.conversation_id);
        Ok(response)
    }

    fn pending_confirmation_count(&self, conversation_id: &str) -> usize {
        self.task_manager
            .get_task(conversation_id)
            .map(|agent| agent.get_confirmations().len())
            .unwrap_or(0)
    }

    async fn resolve_agent_icon(&self, agent: &TeamAgent) -> Result<Option<String>, TeamError> {
        if let Some(assistant_id) = agent.assistant_id.as_deref()
            && let Some(icon) = self
                .assistant_catalog
                .resolve_team_selectable_assistant(assistant_id)
                .await?
                .and_then(|assistant| assistant.avatar)
        {
            return Ok(Some(icon));
        }

        if let Some(row) = self
            .agent_metadata_repo
            .find_builtin_by_backend(agent.backend.as_str())
            .await?
            && row.icon.is_some()
        {
            return Ok(row.icon);
        }

        if agent.backend == "acp"
            && let Some(row) = self
                .agent_metadata_repo
                .find_builtin_by_backend(agent.model.as_str())
                .await?
        {
            return Ok(row.icon);
        }

        Ok(None)
    }
}
