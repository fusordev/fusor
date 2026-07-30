import supportDefault, {
    answer as importedAnswer,
} from "./module-forms-support.mjs";
import * as supportNamespace from "./module-forms-support.mjs";
import "./support.mjs";

const local = supportDefault + importedAnswer + supportNamespace.answer;

export { local, importedAnswer as renamedAnswer };
export { answer as reexportedAnswer } from "./module-forms-support.mjs";
export * from "./module-forms-support.mjs";
export * as namespace from "./module-forms-support.mjs";
export default local;
