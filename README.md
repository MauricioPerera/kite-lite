# kite-lite

Motor web ligero, autocontenido y orientado a agentes. Está diseñado para
ejecutarse fuera de Cloudflare en un VPS o contenedor, con un parser HTML,
JavaScript aislado, renderizado SVG determinista y una superficie CDP pequeña.

## Estado actual

El núcleo DOM en Rust:

- parsea HTML con un parser compatible con HTML5;
- extrae título, texto y enlaces en formato JSON, resolviendo cada enlace
  relativo a una URL absoluta contra la URL final de la página;
- mantiene una representación de árbol preparada para añadir CSS, JavaScript y renderizado;
- evita estado global en parseo y evaluación local;
- conserva la URL final (post-redirecciones) y el HTML fuente dentro de una sesión CDP;
- mantiene cookies entre navegaciones de una misma sesión (`fetch`/CDP)
  mediante un cliente HTTP con cookie jar propio.

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
CLI additionally runs page scripts in a separate `kite-lite-js` process and
terminates it after 1.5 seconds. That evaluator binary does not link
`reqwest`/`tokio` at all, so it cannot make network requests regardless of
what a script tries to do — the isolation is a property of what's compiled
into the executable, not just of what the code chooses to call. This is a
useful local isolation boundary; production deployments should still add
OS-level resource limits and containers or microVMs.

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
ya serializadas, se puede ejecutar el evaluador sin red — y desde que el
evaluador (`kite-lite-js`) es un binario separado que no enlaza `reqwest` ni
`tokio`, `--network=none` es una garantía real del binario, no solo de la
configuración del contenedor:

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
- `POST /v1/parse` con HTML en el body (los enlaces quedan relativos, ya que
  este endpoint no recibe una URL base para resolverlos);
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

`Page.navigate` y `Page.reload` descargan HTML con un cliente HTTP persistente
por sesión CDP (la vida del proceso `cdp` en ejecución), reconstruyen el DOM y
actualizan la URL con el destino final tras seguir redirecciones — no con la
URL originalmente solicitada. Las cookies que el sitio establezca (incluso en
saltos intermedios de una redirección) se guardan en el cookie jar de esa
sesión y se reenvían en navegaciones posteriores dentro del mismo proceso
`cdp`, tal como esperaría un agente que necesite, por ejemplo, permanecer
autenticado entre una página de login y las siguientes. `Runtime.evaluate`
sólo expone un snapshot reducido del documento; no proporciona `fetch`,
filesystem ni bindings del host.

Ejemplo conceptual de una llamada CDP:

```json
{"id":1,"method":"Page.navigate","params":{"url":"https://example.com"}}
{"id":2,"method":"DOM.querySelector","params":{"nodeId":1,"selector":"h1"}}
{"id":3,"method":"DOM.getOuterHTML","params":{"nodeId":6}}
```

La implementación todavía no ofrece layout real, eventos de entrada, captura
PNG/PDF, ejecución de scripts de la página ni eventos de red. Tampoco modela
múltiples pestañas/targets: cada proceso `cdp` sirve un único `Page`
compartido por todas las conexiones WebSocket que se le hagan.

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

1. agregar estilos computados y layout mínimos;
2. implementar interacción DOM y eventos de entrada;
3. renderizar a PNG/PDF;
4. ampliar la compatibilidad con Playwright/MCP.
