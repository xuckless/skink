// Docs sections, in reading order. Shared by the docs overview, llms.txt and
// the home page.
export const SECTIONS = [
  { dir: 'start', name: 'Get started', about: 'Install Skink, learn the few keys you need, quit safely.' },
  { dir: 'editing', name: 'Everyday editing', about: 'Mouse, multiple cursors, find and replace, splits, zoom.' },
  { dir: 'code', name: 'Code and projects', about: 'Language servers, project search, refactoring, tasks.' },
  { dir: 'tools', name: 'Terminal, Git and AI', about: 'Terminal tabs, git, AI agents, Skink as $EDITOR.' },
  { dir: 'remote', name: 'Remote', about: 'Edit on servers over SSH with your clipboard intact.' },
  { dir: 'customize', name: 'Customize', about: 'settings.json, key bindings, the macOS Option key.' },
  { dir: 'reference', name: 'Reference', about: 'Every shortcut, command and manual topic.' },
] as const;

export const MORE = [
  { dir: 'compare', name: 'Compared with other editors', about: 'Honest comparisons with Neovim, Vim, nano, Micro, Helix, Zed and VS Code Remote.' },
  { dir: 'articles', name: 'Articles', about: 'Answers to common terminal-editor questions.' },
] as const;

export type Sectioned = { id: string; data: { title: string; description?: string; sidebar?: { order?: number; label?: string } } };

export function inSection<T extends Sectioned>(docs: T[], dir: string): T[] {
  return docs
    .filter((d) => d.id.startsWith(dir + '/'))
    .sort((a, b) => (a.data.sidebar?.order ?? 99) - (b.data.sidebar?.order ?? 99) || a.data.title.localeCompare(b.data.title));
}
