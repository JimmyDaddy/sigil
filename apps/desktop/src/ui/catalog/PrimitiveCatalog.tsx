import { useState } from "react";

import { PaginationControl } from "../feedback";
import { Icon } from "../icons";
import {
  Button,
  Checkbox,
  Collapsible,
  Dialog,
  Drawer,
  IconButton,
  Menu,
  MenuItem,
  Popover,
  Select,
  TextArea,
  TextField,
  Toast,
  Tooltip,
} from "../primitives";

/** Development-only representative states for the internal primitive contract. */
export function PrimitiveCatalog() {
  const [dialogOpen, setDialogOpen] = useState(false);
  const [drawerOpen, setDrawerOpen] = useState(false);
  return (
    <section data-primitive-catalog="sigil-desktop-dev-ui-catalog">
      <Button variant="primary" onClick={() => setDialogOpen(true)}>Open dialog</Button>
      <Button onClick={() => setDrawerOpen(true)}>Open drawer</Button>
      <IconButton aria-label="Add" icon={<Icon name="add" />} />
      <TextField label="Search" description="Search saved conversations" />
      <TextArea label="Prompt" error="Representative error" />
      <Select label="State"><option>Ready</option><option>Running</option></Select>
      <Checkbox label="Pinned" />
      <Popover label="Filters"><Checkbox label="Only actionable" /></Popover>
      <Menu label="Actions"><MenuItem>Open</MenuItem><MenuItem disabled>Unavailable</MenuItem></Menu>
      <Tooltip label="Nonessential detail"><span tabIndex={0}>Focus for hint</span></Tooltip>
      <Collapsible label="Evidence" summary="2 items">Receipt and snapshot</Collapsible>
      <PaginationControl label="Load more" loadingLabel="Loading" loading={false} onLoadMore={() => undefined} />
      <PaginationControl label="Load more" loadingLabel="Loading" loading onLoadMore={() => undefined} />
      <Toast tone="success" title="Done" timeoutMs={4_000} onDismiss={() => undefined}>Saved locally</Toast>
      <Dialog open={dialogOpen} title="Representative dialog" onOpenChange={setDialogOpen}><Button>Continue</Button></Dialog>
      <Drawer open={drawerOpen} title="Representative drawer" onOpenChange={setDrawerOpen}><Button>Session row</Button></Drawer>
    </section>
  );
}
