- Use Nix flakes for any dependencies
- Use ../direnv as product reference - not code reference
- Never read the source code of a repository that is not permissively
  licensed. Permissive means MIT, BSD, ISC, Apache-2.0, Zlib, Unlicense or
  CC0; anything else (GPL, LGPL, AGPL, MPL, "Other", or no license at all)
  is off limits. Check the license (repository metadata or its LICENSE file)
  before opening any source file. Public docs, man pages, READMEs and issue
  threads of such projects may be read, to learn behavior only. Known
  off-limits references: Flox (GPL-2.0), smartcd (GPL-2.0/Artistic),
  zsh-autoenv (no license).
- Code adapted from a permissively licensed project must keep its copyright
  notice and be credited where it is used.
- foldl is MIT licensed.
- Commits follow Conventional Commits (`feat:`, `fix:`, `docs:`, `test:`,
  `refactor:`, `chore:`, ...). Titles are at most 50 columns. Every commit
  has a body the length of one or two tweets, wrapped at 72 columns,
  explaining why the change was made.
- Work is planned and tracked in beads (label `repo:foldl`), not in markdown
  files. The MVP is the beads epic "foldl MVP"
  (`agents-7pw`); start with `bd ready -l repo:foldl`.
