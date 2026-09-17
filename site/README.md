# zbrew site

The zbrew website. It is a [Hugo](https://gohugo.io) site, built and deployed to
GitHub Pages by [`.github/workflows/hugo.yml`](../.github/workflows/hugo.yml).

## Local development

Install Hugo (extended, 0.128.0 or newer — the CI workflow pins 0.128.0), then
run one of:

```bash
hugo server --source site        # live preview on http://localhost:1313/zbrew/
hugo --source site --minify      # one-off build into site/public
```

Both commands work from the repository root. CI runs the second one with
`--baseURL` set to the Pages URL.

## Layout

| Path | Contents |
|---|---|
| `hugo.toml` | Site config: base URL, title, description, nav |
| `content/_index.md` | Home page front matter (the markup lives in `layouts/index.html`) |
| `content/docs/_index.md` | The docs page |
| `content/{install,discord,github}.md` | Short links; each renders a meta-refresh page |
| `layouts/` | Templates and partials |
| `data/` | Home page copy: hero, install commands, benchmark numbers |
| `static/assets/` | CSS, JS, images, favicon — copied verbatim into the build |

## Editing the home page

The home page copy is data, not markup. Change `data/home.json`,
`data/install.json` or `data/benchmarks.json` and the page follows.

## Short links

`/install`, `/discord` and `/github` are redirect pages. Each one is a content
file with `type: redirect` and a `params.target` URL; `layouts/redirect/` turns
that into a `<meta http-equiv="refresh">` page.

These are browser redirects, so `curl https://…/install | bash` downloads the
redirect page rather than following it. Point install instructions at the raw
`install.sh` URL directly.
