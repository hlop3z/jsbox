# Container Setup

This guide shows how to run **jsbox** using Docker Compose.

---

## 1. Download configuration files

Download only the required runtime files:

```sh
curl -O https://raw.githubusercontent.com/hlop3z/jsbox/main/container/docker-compose.yml
curl -O https://raw.githubusercontent.com/hlop3z/jsbox/main/container/config.json
```

---

## 2. (Optional) Review configuration

The default `config.json` controls engine limits and server binding:

- Execution time limits
- Memory / stack constraints
- Script size restrictions
- Concurrency settings

You can safely adjust it before starting the service.

---

## 3. Start the service

Run the container in detached mode:

```sh
docker compose up -d
```

The API will be available at:

```
http://localhost:4172
```

---

## 4. Verify the service

Send a test execution request:

```sh
curl -X POST http://localhost:4172/execute \
  -H "Content-Type: application/json" \
  -d '{
    "script": "function handler(ctx) { return json({ greeting: \"hello \" + ctx.name }, null); }",
    "context": { "name": "Alice" }
  }'
```

---

## 5. Expected response

```json
{
  "data": { "greeting": "hello Alice" },
  "error": null,
  "meta": {
    // ...
  }
}
```

---

## Writing handler scripts (autocomplete + type-checking)

This folder ships `types.d.ts` and `tsconfig.json` so your editor gives you
**autocomplete and type-checking** for the sandbox globals (`json`, `$`,
`api`, `db`, `mail`, `s3`) with zero setup — just write a `.js` file here:

```js
/** @type {Handler} */
function handler(ctx) {
  const usage = s3.usage({ prefix: "user-a/" });
  return json({ bytes: usage.bytes, files: usage.objects }, null);
}
```

Open the file in VS Code (or any TypeScript-aware editor) and `s3.`, `db.`,
`ctx.`, etc. autocomplete; mistakes are flagged inline. Nothing is compiled —
jsbox runs your `.js` as-is in QuickJS; `tsconfig.json` is editor-only
(`noEmit`). Keep one handler script per file at the top level (each declares a
global `handler`). The `tests/` examples are excluded for that reason.

> `api`, `db`, `mail`, and `s3` are typed as always-present for convenience, but
> at runtime each exists only when you send its `config` block — guard optional
> ones with `typeof`.

## Notes

- Ensure Docker is running before starting Compose
- Port `4172` is mapped to the container’s internal server port
- Modify `config.json` if you need to tune performance or safety limits
- Restart after config changes:

```sh
docker compose restart
```

---

## Optional: Clean restart (fresh state)

```sh
docker compose down
docker compose up -d
```
