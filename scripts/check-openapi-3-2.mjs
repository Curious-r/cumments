import { readFileSync } from "node:fs";

const contract = readFileSync("docs/openapi.yaml", "utf8");

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

assert(/^\nopenapi: 3\.2\.0\n/.test(`\n${contract}`), "contract must declare OpenAPI 3.2.0");
assert(
  /^\$self: https:\/\/cumments\.curious\.host\/openapi\.yaml$/m.test(contract),
  "contract must declare $self",
);
assert(!/^.*nullable:.*$/m.test(contract), "contract must not use OAS 3.0 nullable");
assert(!/x-query|x-cumments-query/.test(contract), "contract must use native QUERY operations");

const queryOperations = [...contract.matchAll(/^ {4}query:$/gm)].length;
assert(queryOperations === 3, `expected 3 native query operations, found ${queryOperations}`);
assert(
  /^\s+itemSchema:\n\s+\$ref: "#\/components\/schemas\/CommentSseFrame"$/m.test(contract),
  "SSE response must describe each parsed event frame with itemSchema",
);
assert(
  /contentMediaType: application\/json/.test(contract) &&
    /contentSchema:/.test(contract),
  "JSON-valued SSE data must declare contentMediaType and contentSchema",
);

for (const operationId of ["queryComments", "listOperatorSites", "listQuarantinedRooms"]) {
  assert(new RegExp(`^      operationId: ${operationId}$`, "m").test(contract), `missing ${operationId}`);
}
