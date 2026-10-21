//! Qual animação tocar: estado dos agentes por baixo, interação com o mouse por cima.
//! Porte de `selectPetAnimationName`/`nextPetDragAnimation` (Orca, alinhado com o Codex).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    Waiting,
    Done,
    Failed,
}

impl AgentState {
    pub fn animation(self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "running",
            AgentState::Waiting => "waiting",
            AgentState::Done => "review",
            AgentState::Failed => "failed",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentState::Idle => "parado",
            AgentState::Working => "trabalhando",
            AgentState::Waiting => "esperando você",
            AgentState::Done => "terminou",
            AgentState::Failed => "falhou",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    None,
    Right,
    Left,
}

/// Direção só muda com deslocamento horizontal de 4 px; `accepted` diz se a base deve avançar,
/// para arrastos lentos na diagonal ainda acumularem.
pub fn next_drag(current: Drag, delta_x: i32) -> (Drag, bool) {
    if delta_x >= 4 {
        (Drag::Right, true)
    } else if delta_x <= -4 {
        (Drag::Left, true)
    } else {
        (current, false)
    }
}

/// Arrastar na horizontal corre na direção do ponteiro; segurar parado mantém o estado dos agentes;
/// só o hover simples pula.
pub fn select(state: AgentState, dragging: bool, drag: Drag, hovering: bool) -> &'static str {
    if dragging {
        return match drag {
            Drag::Right => "running-right",
            Drag::Left => "running-left",
            Drag::None => state.animation(),
        };
    }
    if hovering {
        return "jumping";
    }
    state.animation()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_direction_needs_four_pixels() {
        assert_eq!(next_drag(Drag::None, 3), (Drag::None, false));
        assert_eq!(next_drag(Drag::None, 4), (Drag::Right, true));
        assert_eq!(next_drag(Drag::Right, -4), (Drag::Left, true));
        assert_eq!(next_drag(Drag::Left, 1), (Drag::Left, false));
    }

    #[test]
    fn interaction_overrides_agent_state() {
        assert_eq!(
            select(AgentState::Working, false, Drag::None, false),
            "running"
        );
        assert_eq!(
            select(AgentState::Waiting, false, Drag::None, true),
            "jumping"
        );
        assert_eq!(
            select(AgentState::Waiting, true, Drag::None, true),
            "waiting"
        );
        assert_eq!(
            select(AgentState::Idle, true, Drag::Left, true),
            "running-left"
        );
    }
}
