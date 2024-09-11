// This is the draft of a script meant to automate running soak tests
// It depends on the gh cli tool being configured on your machine

// RUN WORKFLOW
const args: Record<string, string | number | boolean> = {
  environment: "tools",
  executors: 1,
  maxDuration: 10,
  rate: 1000,
  test: "create_and_delete_component",
  useJitter: true,
};

const cmd = "gh";

const ghArgs = [];
for (const argName in args) {
  const value = args[argName];
  ghArgs.push(`-f${argName}=${value}`);
}

console.log(ghArgs);

const runCommand = new Deno.Command(cmd, {
  args: [
    "workflow",
    "run",
    "soak-test.yml",
    ...ghArgs,
  ],
});
const runOutput = await runCommand.output();
console.assert(runOutput.code === 0);

console.log(new Date());

// CHECK WORKFLOW
// FIXME this is running too fast and the task isn't showing up on the list
const listCommand = new Deno.Command(cmd, {
  args: [
    "run",
    "list",
    "-wsoak-test.yml",
    "--json",
    "databaseId,status,createdAt",
    "-L1",
  ],
});

const listOutput = await listCommand.output();
console.assert(listOutput.code === 0);
const runsRaw = new TextDecoder().decode(listOutput.stdout);
const runs = JSON.parse(runsRaw);
console.log(runs);

const run = runs[0];
console.log(run?.status);
