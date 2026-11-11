import ref from '../data/reference.json';

export const manualSlug = (h: string) =>
  h.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

export const anchor = (action: string) => action.split(/[|,]/)[0].replace(/[^A-Za-z]/g, '');

const GROUPS: [string, RegExp][] = [
  ['AI and agents', /Agent|^Ai[A-Z]|Review/],
  ['Debugger', /Debug|Breakpoint/],
  ['Terminal', /Prompt$|LastOutput|TerminalHistory|Terminal$|TerminalDock/],
  ['Git', /Hunk|Blame|Git|Conflict|Stage|Rebase/],
  ['Code intelligence', /Definition|Reference|Hover|Rename|Format|Autocomplete|CodeAction|Symbol|Problem|Diagnostic|Implementation|Signature|Lsp/],
  ['Search', /Find|Search|Replace|Grep/],
  ['Tabs, panes and layout', /Tab|Terminal|Tree|Sidebar|Pane|Split|Zoom|Dock|Hidden|Palette|QuickOpen|Open|Quit|Font|Fold|Task|Macro|Back|Forward|Prompt|Scrollback|History/],
  ['Selecting', /^Select|MultiCursor|Expand|Shrink/],
  ['Moving around', /Cursor|Word(Left|Right)|Start|End|Page|Scroll|Jump|Line(Up|Down)$|Paragraph/],
];

export function groupKeys() {
  const out = new Map<string, typeof ref.keys>();
  for (const [g] of GROUPS) out.set(g, []);
  out.set('Editing', []);
  for (const k of ref.keys) {
    const g = GROUPS.find(([, re]) => re.test(k.action))?.[0] ?? 'Editing';
    out.get(g)!.push(k);
  }
  return [...out].filter(([, v]) => v.length);
}

export { ref };

// The manual is split into one page per topic, grouped so the index is not a
// wall of 30+ links. Topics not listed here land in "More".
const MANUAL_GROUPS: [string, string, RegExp][] = [
  ['Editing', 'Typing, selecting, cursors and the command line.', /^(Getting started|Editing|Typing helpers|Lines|Multiple cursors|Macros|Command line|Typed commands)/],
  ['Files and layout', 'Tabs, the file tree, panes, sessions and views.', /^(Tabs|File tree|Split panes|Views|Sessions|Folding|Jumps|Files on disk|Zoom|Previews)/],
  ['Code', 'Language servers, search, git, tasks and the debugger.', /^(Code intelligence|Languages|Search in files|Tasks|Debugging|Merge conflicts|Git)/],
  ['Terminal and AI', 'Shells, agents and Skink as your $EDITOR.', /^(Terminals|AI|Skink as your|Remote)/],
  ['Setup', 'Settings, themes, packages, scripts and updates.', /^(Settings|Themes|Packages|Scripts|Updates|macOS|Icons)/],
];

export type ManualTopic = { heading: string; slug: string; body: string; lead: string };

export const manualTopics: ManualTopic[] = ref.manual.map((m) => ({
  heading: m.heading,
  slug: manualSlug(m.heading),
  body: m.body,
  lead: m.body.split('\n\n')[0].replace(/\s+/g, ' ').trim(),
}));

export function manualGroups() {
  const groups = MANUAL_GROUPS.map(([name, about, re]) => ({ name, about, topics: manualTopics.filter((t) => re.test(t.heading)) }));
  const seen = new Set(groups.flatMap((g) => g.topics));
  const rest = manualTopics.filter((t) => !seen.has(t));
  if (rest.length) groups.push({ name: 'More', about: 'Everything else.', topics: rest });
  return groups.filter((g) => g.topics.length);
}

export const manualUrl = (heading: string) => `/reference/manual/${manualSlug(heading)}/`;
