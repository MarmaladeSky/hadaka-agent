#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandId {
    Settings,
    Exit,
}

pub(super) struct Command {
    pub(super) id: CommandId,
    pub(super) name: &'static str,
    pub(super) description: &'static str,
}

impl Command {
    pub(super) fn label(&self) -> String {
        format!("/{}", self.name)
    }
}

pub(super) const COMMANDS: &[Command] = &[
    Command {
        id: CommandId::Settings,
        name: "settings",
        description: "Configure providers and application settings",
    },
    Command {
        id: CommandId::Exit,
        name: "exit",
        description: "Exit the chat session",
    },
];

pub(super) fn find(input: &str) -> Option<&'static Command> {
    let name = input.trim().strip_prefix('/')?;
    COMMANDS
        .iter()
        .find(|command| command.name.eq_ignore_ascii_case(name))
}

// Each command appears once, in its strongest group, keeping registry order.
pub(super) fn search(query: &str) -> Vec<&'static Command> {
    let query = query.trim().to_lowercase();
    let mut results = Vec::new();
    for group in 0..3 {
        results.extend(
            COMMANDS
                .iter()
                .filter(|command| rank(command, &query) == Some(group)),
        );
    }
    results
}

fn rank(command: &Command, query: &str) -> Option<usize> {
    let name = command.name.to_lowercase();
    if name.starts_with(query) {
        Some(0)
    } else if name.contains(query) {
        Some(1)
    } else if command.description.to_lowercase().contains(query) {
        Some(2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(query: &str) -> Vec<String> {
        search(query)
            .iter()
            .map(|command| command.label())
            .collect()
    }

    #[test]
    fn ranks_name_prefix_then_substring_then_description() {
        assert_eq!(labels(""), ["/settings", "/exit"]);
        assert_eq!(labels("s"), ["/settings", "/exit"]);
        assert_eq!(labels("xi"), ["/exit"]);
        assert_eq!(labels("session"), ["/exit"]);
        assert_eq!(labels("providers"), ["/settings"]);
        assert!(labels("nothing here").is_empty());
    }

    #[test]
    fn matches_without_case_and_lists_each_command_once() {
        assert_eq!(labels("SET"), ["/settings"]);
        assert_eq!(labels("ExIt"), ["/exit"]);
        // "e" is a name prefix of /exit and also appears in both descriptions.
        assert_eq!(labels("e"), ["/exit", "/settings"]);
    }

    #[test]
    fn finds_exact_commands_only() {
        assert_eq!(find("/settings").map(|c| c.id), Some(CommandId::Settings));
        assert_eq!(find("  /EXIT  ").map(|c| c.id), Some(CommandId::Exit));
        assert!(find("/set").is_none());
        assert!(find("settings").is_none());
        assert!(find("/unknown").is_none());
    }
}
