// A dialog whose actions report how many times they were built.
import { mount, Text } from "nsh";

import { Dialog } from "../../src/ui/dialog";

let built = 0;

/** Counts its own constructions in its text. */
function Probe() {
  built += 1;
  return <Text>{`built ${built}`}</Text>;
}

mount(() => (
  <Dialog open title="Probe" onClose={() => {}} actions={<Probe />}>
    <Text>Body</Text>
  </Dialog>
));
