//! Qual animação tocar: estado dos agentes e o que o pet percebe por baixo, interação com o mouse
//! por cima.
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

/// O que o pet percebe do computador fora os agentes (`activity.rs`), já filtrado pelo que o
/// usuário deixou ligado no menu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    pub meeting: bool,
    pub away: bool,
    pub coding: bool,
}

/// O que o pet mostra: os agentes e o que ele percebe, juntos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Agent(AgentState),
    Meeting,
    Away,
    Coding,
}

impl Mood {
    /// Um agente esperando você > reunião > outro estado de agente > ausência > programando.
    /// Na reunião o pet só não esconde um pedido do Claude ou do Codex.
    pub fn of(state: AgentState, activity: Activity) -> Mood {
        if state == AgentState::Waiting {
            Mood::Agent(state)
        } else if activity.meeting {
            Mood::Meeting
        } else if state != AgentState::Idle {
            Mood::Agent(state)
        } else if activity.away {
            Mood::Away
        } else if activity.coding {
            Mood::Coding
        } else {
            Mood::Agent(AgentState::Idle)
        }
    }

    pub fn animation(self) -> &'static str {
        match self {
            Mood::Agent(state) => state.animation(),
            Mood::Meeting | Mood::Away => "idle",
            Mood::Coding => "running",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mood::Agent(state) => state.label(),
            Mood::Meeting => "em reunião",
            Mood::Away => "cochilando",
            Mood::Coding => "programando com você",
        }
    }

    /// Parado num quadro só, para não chamar atenção; o timer de quadros desliga.
    pub fn frozen(self) -> bool {
        matches!(self, Mood::Meeting | Mood::Away)
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

/// Arrastar na horizontal corre na direção do ponteiro; segurar parado mantém o estado;
/// só o hover simples pula.
pub fn select(mood: Mood, dragging: bool, drag: Drag, hovering: bool) -> &'static str {
    if dragging {
        return match drag {
            Drag::Right => "running-right",
            Drag::Left => "running-left",
            Drag::None => mood.animation(),
        };
    }
    if hovering {
        return "jumping";
    }
    mood.animation()
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
        let mood = |state| Mood::Agent(state);
        assert_eq!(
            select(mood(AgentState::Working), false, Drag::None, false),
            "running"
        );
        assert_eq!(
            select(mood(AgentState::Waiting), false, Drag::None, true),
            "jumping"
        );
        assert_eq!(
            select(mood(AgentState::Waiting), true, Drag::None, true),
            "waiting"
        );
        assert_eq!(
            select(mood(AgentState::Idle), true, Drag::Left, true),
            "running-left"
        );
    }

    #[test]
    fn agent_waiting_beats_meeting_and_meeting_beats_the_rest() {
        let all = Activity {
            meeting: true,
            away: true,
            coding: true,
        };
        assert_eq!(
            Mood::of(AgentState::Waiting, all),
            Mood::Agent(AgentState::Waiting)
        );
        assert_eq!(Mood::of(AgentState::Working, all), Mood::Meeting);
        assert_eq!(Mood::of(AgentState::Idle, all), Mood::Meeting);
    }

    #[test]
    fn agents_beat_away_and_away_beats_coding() {
        let away = Activity {
            away: true,
            coding: true,
            ..Activity::default()
        };
        assert_eq!(
            Mood::of(AgentState::Done, away),
            Mood::Agent(AgentState::Done)
        );
        assert_eq!(Mood::of(AgentState::Idle, away), Mood::Away);
        let coding = Activity {
            coding: true,
            ..Activity::default()
        };
        assert_eq!(Mood::of(AgentState::Idle, coding), Mood::Coding);
        assert_eq!(
            Mood::of(AgentState::Idle, Activity::default()),
            Mood::Agent(AgentState::Idle)
        );
    }
}
