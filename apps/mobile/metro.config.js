// Expo's monorepo defaults, plus one rule: the workspace packages
// (@todo-tui/client, @todo-tui/present) are TypeScript source written for
// Node's ESM resolution, so their relative imports name `.js` files that
// exist as `.ts`. Metro does not map one to the other, so a relative `.js`
// import from a file in packages/ is retried as `.ts`, then `.tsx`.
const path = require("node:path");
const { getDefaultConfig } = require("expo/metro-config");

const config = getDefaultConfig(__dirname);
const packages = path.resolve(__dirname, "../../packages") + path.sep;

config.resolver.resolveRequest = (context, moduleName, platform) => {
  if (moduleName.startsWith(".") && moduleName.endsWith(".js") && context.originModulePath.startsWith(packages)) {
    for (const ext of [".ts", ".tsx"]) {
      try {
        return context.resolveRequest(context, moduleName.slice(0, -3) + ext, platform);
      } catch {
        // try the next, then the name as written
      }
    }
  }
  return context.resolveRequest(context, moduleName, platform);
};

module.exports = config;
