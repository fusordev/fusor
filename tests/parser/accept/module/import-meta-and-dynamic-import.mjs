const metadata = import.meta;

function loadLater() {
    return import("./support.mjs", { with: {} });
}

export { loadLater, metadata };
