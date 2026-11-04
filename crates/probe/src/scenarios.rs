//! The guided recording scenarios.

pub struct Scenario {
    /// File stem of the log and the name shown in reports.
    pub id: &'static str,
    pub title: &'static str,
    pub hint: &'static str,
}

pub const SCENARIOS: [Scenario; 4] = [
    Scenario {
        id: "slow_h",
        title: "медленно по горизонтали",
        hint: "Веди шарик очень медленно и равномерно влево и вправо.",
    },
    Scenario {
        id: "slow_v",
        title: "медленно по вертикали",
        hint: "Веди шарик очень медленно и равномерно вверх и вниз.",
    },
    Scenario {
        id: "fast",
        title: "быстрые движения",
        hint: "Крути шарик быстро и резко в разные стороны, без пауз.",
    },
    Scenario {
        id: "circle",
        title: "по кругу",
        hint: "Выписывай курсором плавные круги средней скорости.",
    },
];
