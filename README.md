# cgpt-jsonschema-lsp

A small LSP server that validates:

- `*.json` files by reading the document's top-level `$schema` and validating the
  document against that schema using `Stranger6667/jsonschema`.
- `*.yml` / `*.yaml` files by converting YAML -> JSON first, then validating
  with the same JSON Schema workflow.

It is intended to be used with Vim/YouCompleteMe and the `--stdio` flag.

## Build

```bash
cargo build --release
```

## Run (stdio)

```bash
./target/release/cgpt-jsonschema-lsp --stdio
```

The server **only** writes the LSP protocol to stdout; logs go to stderr.

## Vim / YouCompleteMe example

In `.vimrc` (example, adapt to your setup):

```vim
let g:ycm_language_server =
\ [
\   {
\     'name': 'cgpt-jsonschema-lsp',
\     'filetypes': [ 'json', 'yaml' ],
\     'cmdline': [ '/ABS/PATH/TO/cgpt-jsonschema-lsp', '--stdio' ],
\   }
\ ]
```

## Notes / expectations

- If a file has no top-level `$schema`, the server does not validate it.
- `$schema` can be:
  - `https://...` / `http://...`
  - `file:///...`
  - a local path like `./schemas/foo.schema.json` (resolved relative to the
    edited file)
- For JSON files, schema validation errors are mapped to exact source ranges
  using a JSON AST with byte spans.
- For YAML files, schema errors are reported at the start of the document
  (mapping JSON pointers back to YAML source ranges is non-trivial).
