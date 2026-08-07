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
  mediante un cliente HTTP con cookie jar propio;
- calcula un layout mínimo por elemento (`{x, y, width, height}` en
  `Element.layout`) usando una hoja de estilos por defecto fija por tag
  (tamaños/negrita de h1-h6, márgenes de p/li/blockquote), con ajuste de
  texto (word wrap) — ver la sección "Layout mínimo" para las limitaciones;
  permite click/escritura/submit de formularios por CDP sin ejecutar JS de
  la página — ver "Interacción: click, escritura y submit de formularios";
  renderiza a PNG y PDF además de SVG — ver "Renderizado PNG/PDF";
  se puede correr como servidor MCP por stdio — ver "Servidor MCP".

## Probarlo

```powershell
cargo test
cargo run -- https://example.com
cargo run -- https://example.com --svg example.svg --js "1 + 2"
cargo run -- https://example.com --png example.png
cargo run -- https://example.com --pdf example.pdf
cargo run -- fetch https://example.com --output page.json
cargo run -- eval page.json --js "document.title"
cargo run -- render page.json --output page.svg
cargo run -- render page.json --output page.png
cargo run -- render page.json --output page.pdf
cargo run -- serve 127.0.0.1:8787
cargo run -- cdp page.json 127.0.0.1:9222
cargo run -- cdp 127.0.0.1:9222
```

`render` (and `POST /v1/render`, via `?format=png|pdf|svg`) picks the output
format from the `--output` path's extension (`.svg` is the default for
anything else).

`--js` evaluates a script in a fresh Boa JavaScript context with no filesystem,
network, or host bindings. The page snapshot exposes `document.title`,
`document.body.innerText`, and a limited `document.querySelector()` for the
first `h1`, `h2`, `h3`, `p`, `a`, or `button`. `--svg` writes a deterministic
rendering driven by the minimal layout described below (see "Layout
mínimo"): every leaf element with text gets word-wrapped and positioned
according to a fixed default style per tag.

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

## Layout mínimo

`fetch`, `Page.navigate`/`Page.reload` en CDP, y `POST /v1/parse` calculan un
layout de bloque simple sobre el árbol y lo guardan en `Element.layout`
(`{x, y, width, height}`) antes de devolver el `Page`. `render_svg` vuelve a
calcularlo internamente al ancho exacto que se le pide renderizar (no
reutiliza el guardado en el JSON, para no desalinearse si se pide un ancho
distinto).

Esto **no es un motor de CSS**:

- no hay `<style>` ni `style="..."` ni clases/ids — los tamaños, negrita y
  márgenes son fijos por nombre de tag (h1-h6, p, li, blockquote, strong/b),
  una imitación mínima de una hoja de estilos de usuario por defecto;
- no hay flujo inline real: cualquier elemento con hijos apila sus hijos
  verticalmente a ancho completo. Esto es una limitación real del árbol DOM
  actual, no solo una simplificación: el parser mezcla el texto suelto de un
  elemento con el texto de sus hijos en un único campo `text` (sin
  conservar el orden ni los nodos de texto como hermanos), así que texto
  suelto junto a un hijo — por ejemplo `<p>Hola <a href="/x">link</a></p>` —
  no tiene dónde calcularse por separado una vez que el padre tiene un hijo
  elemento; solo el texto propio de los nodos hoja se ajusta (word wrap) y
  se dibuja;
- `x` siempre es `0.0`: no hay eje horizontal en el layout, solo apilado
  vertical;
- no hay colapso de márgenes ni floats ni posicionamiento.

## Renderizado PNG/PDF

`render_svg` se puede rasterizar a PNG (`resvg` + `tiny-skia`, Rust puro, sin
depender de un navegador ni de herramientas externas) y ese PNG se puede
envolver en un PDF de una sola página (`printpdf`).

Esto **necesita fuentes instaladas en el sistema donde corre el binario**:
todo lo que dibuja `render_svg` es texto (no hay rectángulos, imágenes ni
otros gráficos), y el rasterizador usa `fontdb::load_system_fonts()` — no
trae ninguna fuente embebida. Sin fuentes, el PNG/PDF sale con el texto
faltante (el resto de los elementos, al no existir, tampoco aparece). La
imagen Docker instala `fonts-dejavu-core` para esto; en un build local fuera
de Docker, depende de que el sistema operativo tenga alguna fuente
disponible.

El PDF **no es vectorial**: es la imagen PNG rasterizada, envuelta en una
página PDF a 96 DPI — no hay texto seleccionable ni buscable. Un PDF
vectorial de verdad necesitaría su propio pipeline de fuentes independiente
del rasterizado (`printpdf` trae soporte nativo de SVG, pero internamente
usa una versión vieja de `usvg` con `Options::default()` sin fuentes
cargadas y sin forma de inyectar una — no es viable para texto).

## Control API local

El servidor se enlaza a loopback por defecto y expone:

- `GET /health`
- `POST /v1/parse` con HTML en el body (los enlaces quedan relativos, ya que
  este endpoint no recibe una URL base para resolverlos);
- `POST /v1/render` con un snapshot JSON — `?format=svg` (default), `png` o
  `pdf`;
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
- `DOM.getOuterHTML`, `DOM.getAttributes`, `DOM.getBoxModel`;
- `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`;
- `Target.getTargets`, `Target.getTargetInfo`, `Target.attachToTarget`,
  `Target.attachToBrowserTarget`, `Target.setAutoAttach`,
  `Target.setDiscoverTargets`, `Target.closeTarget`,
  `Target.disposeBrowserContext` — un solo target fijo, sin multi-pestaña
  real (ver "Compatibilidad CDP" más abajo);
- eventos básicos `Page.frameStartedLoading`, `Page.loadEventFired` y
  `Page.frameStoppedLoading` (se disparan tanto tras `Page.navigate`/`reload`
  como tras un click que termina navegando), y `Target.attachedToTarget`.

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

La implementación todavía no ofrece captura PNG/PDF vía CDP (usá `render` o
`/v1/render?format=png|pdf` para eso — ver "Renderizado PNG/PDF"), ejecución
de scripts de la página ni eventos de red.

## Compatibilidad CDP

El servidor `cdp` expone, en el mismo puerto, tanto el WebSocket como los
endpoints HTTP de descubrimiento que Chrome real expone en su puerto de
remote-debugging (`GET /json/version`, `GET /json`, `GET /json/list`) —
los mismos que herramientas genéricas (`chrome-remote-interface`, scripts
que arrancan pidiendo `webSocketDebuggerUrl`, etc.) consultan antes de
conectar el WebSocket. También implementa lo mínimo del dominio `Target`
(`getTargets`, `attachToTarget`, `setAutoAttach`, ...) para que un cliente
que espera el flujo real de CDP (adjuntarse a un target antes de operar
sobre él, y que cada respuesta/evento de página lleve `sessionId`) no se
quede esperando algo que nunca llega — pero es un solo target fijo,
simulado: no hay múltiples pestañas/targets reales, `attachToTarget`
siempre devuelve el mismo `sessionId` y sigue siendo el único `Page`
compartido por todas las conexiones WebSocket, como ya se explica arriba.

**Esto NO habilita Playwright real.** `playwright.chromium.connectOverCDP()`
podría llegar a conectar y adjuntarse gracias a esto, pero cada acción de
Playwright (`page.click()`, `page.fill()`, `locator()`) ejecuta JavaScript
inyectado contra un DOM vivo (vía `Runtime.callFunctionOn`) para chequear
visibilidad/scroll/actionability antes de actuar — eso exige exactamente el
DOM↔JS enlazado y persistente que este proyecto evita a propósito (ver
"Interacción" y "Layout mínimo" para el porqué). No se intentó simular eso;
haría creer que funciona hasta el primer click real.

## Interacción: click, escritura y submit de formularios

`Input.dispatchMouseEvent` (`type: "mousePressed"`) y `Input.dispatchKeyEvent`
(`type: "char"`/`"keyDown"`) implementan una interacción **sin ejecutar JS de
la página** — como un navegador con JavaScript desactivado:

- click en un nodo cuya caja de layout contiene la `y` del evento:
  - `<a href>` → navega a esa URL (mismo camino que `Page.navigate`: cookies,
    redirecciones y resolución de URL de la sesión aplican igual);
  - `<input>`/`<textarea>` → lo enfoca, para que `Input.dispatchKeyEvent`
    sepa dónde escribir (el foco se pierde en cualquier navegación);
  - `<button>`, o `<input type="submit">` → busca el `<form>` ancestro más
    cercano, junta el `name`/valor actual de sus `<input>`/`<textarea>`
    descendientes en una query string y navega a `action?query` (o a la URL
    actual si no hay `action`).
- `Input.dispatchKeyEvent` con `type:"char"` agrega `text` al `value` del
  nodo enfocado; con `type:"keyDown"` y `key:"Backspace"` borra el último
  carácter.

Ningún click ejecuta `onclick` ni corre `<script>` de la página — sigue sin
haber un DOM vivo ligado a JS, por las mismas razones que en "Layout mínimo".
Limitaciones adicionales: el click es por coordenada `y` únicamente (no hay
eje `x` en el layout — ver "Layout mínimo"); solo se arma un submit `GET`
(el `method`/`action` con POST no se soporta, no hay cuerpo de request);
`<select>`/checkboxes/radios no tienen semántica propia, se tratan como
cualquier otro nodo sin acción especial. Para saber dónde clickear, un
cliente CDP real primero hace `DOM.querySelector` y después
`DOM.getBoxModel` para obtener las coordenadas — igual que Playwright/Chrome
DevTools.

## Servidor MCP

`cargo run -- mcp` corre kite-lite como servidor MCP (Model Context Protocol)
por stdio: JSON-RPC 2.0 delimitado por saltos de línea en stdin/stdout, el
mismo transporte que usan Claude Desktop/Claude Code para lanzar herramientas
locales. Implementado a mano con `serde_json` (sin SDK de MCP) — misma
filosofía que el resto del proyecto con CDP: la superficie necesaria
(`initialize`, `tools/list`, `tools/call`) es chica.

Herramientas expuestas, todas sobre el motor ya existente:

- `fetch_page(url)` — resumen liviano `{url, title, text, links}`, **no**
  el árbol DOM completo con layout (sería demasiado JSON para el contexto
  de un agente). No ejecuta JS de la página ni toca la sesión persistente.
- `render_screenshot(url, format?)` — trae la URL y la renderiza a PNG
  (imagen en base64) o SVG (texto). Mismas limitaciones que "Renderizado
  PNG/PDF": sin fuentes en el sistema, sale en blanco.
- `eval_js(url, script)` — trae la URL y evalúa JS aislado contra el
  snapshot, mismo sandbox de siempre (sin red/filesystem/DOM real).
- `browser_navigate(url)`, `browser_click(selector)`, `browser_type(text, selector?)`,
  `browser_get_dom(selector?)`, `browser_screenshot(format?)` — una única
  sesión de navegación persistente por proceso (como `cdp`, no
  multi-pestaña), con cookies/redirecciones igual que el resto del
  proyecto. `browser_click` reusa la misma lógica de
  "Interacción: click, escritura y submit de formularios" pero ubicando el
  elemento por selector en vez de coordenada `y` — más natural para una
  herramienta MCP.

Un error de una herramienta (selector que no matchea, fetch fallido, etc.)
se devuelve como `isError: true` con el mensaje en el contenido — así el
agente lo ve y puede reaccionar, en vez de que falle la llamada JSON-RPC
completa.

**Límite honesto:** esto se verificó hablando el protocolo a mano (un
proceso real, mensajes JSON-RPC reales por stdin/stdout, igual rigor que
las pruebas de CDP) — no se probó contra un cliente MCP real (Claude
Desktop/Code) en esta sesión, así que confirma que el protocolo es
correcto, no que un cliente específico lo acepte sin fricción.

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

El roadmap original (fetcher/JS aislados, URLs/cookies/redirecciones,
layout mínimo, interacción, PNG/PDF, compatibilidad CDP, servidor MCP) está
completo. La limitación más grande que queda, y que no tiene una solución
que no rompa el modelo de aislamiento de este proyecto: kite-lite no puede
ver nada que dependa de JavaScript para renderizarse (SPAs, contenido
cargado por fetch del lado cliente) — es la contracara directa de no tener
un DOM vivo ligado a JS, la misma razón por la que ni la interacción real
ni Playwright funcionan. No hay ítem de roadmap para esto porque resolverlo
significaría abandonar esa decisión de diseño, no extenderla.
