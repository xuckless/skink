import ref from '../data/reference.json';

export const manualSlug = (h: string) =>
  h.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

export const anchor = (action: string) => action.split(/[|,]/)[0].replace(/[^A-Za-z]/g, '');

const GROUPS: [string, RegExp][] = [
  ['AI and agents', /Agent|Prompt|Ai[A-Z]|^Ai|Review/],
  ['Git', /Hunk|Blame|Git|Conflict|Stage/],
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
