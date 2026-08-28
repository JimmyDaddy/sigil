use sigil_tui::{App, Damage, InputEvent, NodeKey, Rect, Surface, Text, UpdateOutcome};

#[derive(Default)]
struct Chat {
    draft: String,
}

impl App for Chat {
    type Action = String;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action> {
        match input {
            InputEvent::Paste(value) if !value.is_empty() => {
                self.draft.push_str(&value);
                UpdateOutcome::Redraw(Damage::PAINT)
            }
            InputEvent::Key { code } if code == "enter" && !self.draft.is_empty() => {
                UpdateOutcome::Action(std::mem::take(&mut self.draft))
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
            NodeKey::new("chat.title")?,
            Rect::new(viewport.x, viewport.y, viewport.width, 1),
            Text::new("Chat")?.to_string(),
        )?;
        surface.push_text(
            NodeKey::new("chat.draft")?,
            Rect::new(viewport.x, viewport.y.saturating_add(1), viewport.width, 1),
            self.draft.clone(),
        )?;
        Ok(surface)
    }
}

fn main() -> Result<(), sigil_tui::CoreError> {
    let mut chat = Chat::default();
    assert_eq!(
        chat.handle_input(InputEvent::Paste("hello".into())),
        UpdateOutcome::Redraw(Damage::PAINT)
    );
    assert_eq!(
        chat.handle_input(InputEvent::Key {
            code: "enter".into()
        }),
        UpdateOutcome::Action("hello".into())
    );
    Ok(())
}
