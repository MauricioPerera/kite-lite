# kite-lite

Motor web ligero, autocontenido y orientado a agentes. Está diseñado para
ejecutarse fuera de Cloudflare en un VPS o contenedor, con un parser HTML,
JavaScript aislado, renderizado SVG determinista y una superficie CDP pequeña.

## Estado actual

El núcleo DOM en Rust:

- parsea HTML con un parser compatible con HTML5;
- extrae título, texto y enlaces en formato JSON;
- mantiene una representación de árbol preparada para añadir CSS, JavaScript y renderizado;
- evita estado global en parseo y evaluación local;
- conserva la URL y el HTML fuente dentro de una sesión CDP.

## Probarlo

```powershell
cargo test
cargo run -- https://example.com
cargo run -- https://example.com --svg example.svg --js "1 + 2"
cargo run -- fetch https://example.com --output page.json
cargo run -- eval page.json --js "document.title"
cargo run -- render page.json --output page.svg
cargo run -- serve 127.0.0.1:8787
cargo run -- cdp page.json 127.0.0.1:9222
cargo run -- cdp 127.0.0.1:9222
```

`--js` evaluates a script in a fresh Boa JavaScript context with no filesystem,
network, or host bindings. The page snapshot exposes `document.title`,
`document.body.innerText`, and a limited `document.querySelector()` for the
first `h1`, `h2`, `h3`, `p`, `a`, or `button`. `--svg` writes a deterministic
first-pass rendering of headings, paragraphs, links, lists, and buttons.

Example:

```powershell
cargo run -- https://example.com --js "document.title + ' / ' + document.querySelector('h1').innerText"
```

The JavaScript context now has source-size, recursion, and VM stack limits. The
CLI additionally runs page scripts in a child process and terminates them after
1.5 seconds. This is a useful local isolation boundary; production deployments
should still add OS-level resource limits and containers or microVMs.

## Ejecutar con límites del sistema

La imagen Docker ejecuta el binario como usuario sin privilegios. Para una
prueba con límites estrictos de recursos:

```powershell
docker build -t kite-lite .
docker run --rm `
  --memory=256m --cpus=0.5 --pids-limit=64 `
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m `
  --cap-drop=ALL --security-opt=no-new-privileges `
  kite-lite https://example.com --js "document.title"
```

El acceso de red es necesario en este modo para descargar la URL. Para páginas
ya serializadas, se puede ejecutar el evaluador sin red:

```powershell
docker run --rm --network=none --memory=256m --cpus=0.5 --pids-limit=64 `
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m `
  --cap-drop=ALL --security-opt=no-new-privileges `
  -v "${PWD}/page.json:/input/page.json:ro" `
  kite-lite eval /input/page.json --js "document.title"
```

El renderizado también puede ejecutarse sin red:

```powershell
docker run --rm --network=none --read-only --cap-drop=ALL `
  -v "${PWD}/page.json:/input/page.json:ro" `
  -v "${PWD}:/output" `
  kite-lite render /input/page.json --output /output/page.svg
```

## Control API local

El servidor se enlaza a loopback por defecto y expone:

- `GET /health`
- `POST /v1/parse` con HTML en el body;
- `POST /v1/render` con un snapshot JSON;
- `POST /v1/eval` con `{ "page": ..., "script": "..." }`.

No se debe publicar directamente a Internet; colócalo detrás de autenticación
y un proxy si se necesita acceso remoto.

## CDP experimental

El modo `cdp` abre un WebSocket compatible con una parte pequeña del Chrome
DevTools Protocol:

- `Browser.getVersion`;
- `Runtime.enable`, `Page.enable`, `Network.enable`, `DOM.enable`;
- `Runtime.evaluate`;
- `Page.getNavigationHistory`, `Page.getResourceTree`;
- `Page.navigate`, `Page.reload`, `Page.captureSnapshot`;
- `DOM.getDocument`, `DOM.querySelector`, `DOM.querySelectorAll`;
- `DOM.getOuterHTML`, `DOM.getAttributes`;
- eventos básicos `Page.frameStartedLoading`, `Page.loadEventFired` y
  `Page.frameStoppedLoading`.

Ejecuta `cdp` con un snapshot JSON para iniciar una sesión desde una página
serializada. Sin snapshot inicia una página vacía:

```powershell
cargo run -- cdp page.json 127.0.0.1:9222
```

`Page.navigate` y `Page.reload` descargan HTML con la red del proceso CDP,
reconstruyen el DOM y actualizan la URL. `Runtime.evaluate` sólo expone un
snapshot reducido del documento; no proporciona `fetch`, filesystem ni
bindings del host.

Ejemplo conceptual de una llamada CDP:

```json
{"id":1,"method":"Page.navigate","params":{"url":"https://example.com"}}
{"id":2,"method":"DOM.querySelector","params":{"nodeId":1,"selector":"h1"}}
{"id":3,"method":"DOM.getOuterHTML","params":{"nodeId":6}}
```

La implementación todavía no ofrece layout real, eventos de entrada, cookies,
captura PNG/PDF, ejecución de scripts de la página ni eventos de red.

## Despliegue en VPS

El despliegue probado usa una imagen multi-stage y un usuario sin privilegios:

```bash
docker build -t kite-lite:dev .
docker run --rm \
  --memory=256m --cpus=0.5 --pids-limit=64 \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --cap-drop=ALL --security-opt=no-new-privileges \
  kite-lite:dev cdp 0.0.0.0:9222
```

El puerto CDP debe mantenerse en loopback o protegerse con un túnel y
autenticación. No se recomienda exponerlo directamente a Internet.

## Próximas capas

1. separar fetcher y ejecutor JS para permitir `--network=none`;
2. resolver URLs, cookies y redirecciones por sesión;
3. agregar estilos computados y layout mínimos;
4. implementar interacción DOM y eventos de entrada;
5. renderizar a PNG/PDF;
6. ampliar la compatibilidad con Playwright/MCP.
