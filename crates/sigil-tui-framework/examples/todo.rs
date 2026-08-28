use sigil_tui::{App, Damage, InputEvent, NodeKey, Rect, Surface, Text, UpdateOutcome};

#[derive(Default)]
struct Todo {
    completed: bool,
}

impl App for Todo {
    type Action = String;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action> {
        match input {
            InputEvent::Key { code } if code == "space" => {
                self.completed = !self.completed;
                UpdateOutcome::Redraw(Damage::PAINT)
            }
            InputEvent::Key { code } if code == "enter" => {
                UpdateOutcome::Action("submit-todo".to_owned())
            }
            _ => UpdateOutcome::Ignored,
        }
    }

    fn build_surface(
        &self,
        viewport: Rect,
        generation: u64,
    ) -> Result<Surface, sigil_tui::CoreError> {
        let mut surface = Surface::new(viewport, generation)?;
        surface.push_text(
            NodeKey::new("todo.title")?,
            Rect::new(viewport.x, viewport.y, viewport.width, 1),
            Text::new("Todo")?.to_string(),
        )?;
        surface.push_action(
            NodeKey::new("todo.toggle")?,
            Rect::new(viewport.x, viewport.y.saturating_add(1), viewport.width, 1),
            if self.completed {
                "[x] ship framework"
            } else {
                "[ ] ship framework"
            },
            NodeKey::new("todo.toggle")?,
        )?;
        Ok(surface)
    }
}

fn main() -> Result<(), sigil_tui::CoreError> {
    let mut todo = Todo::default();
    assert_eq!(
        todo.handle_input(InputEvent::Key {
            code: "space".into()
        }),
        UpdateOutcome::Redraw(Damage::PAINT)
    );
    let surface = todo.build_surface(Rect::new(0, 0, 40, 4), 1)?;
    assert_eq!(surface.nodes().len(), 2);
    Ok(())
}
