# persona cards

Each card is a **sealed brief**: it is the only thing an outside agent receives
besides a payload directory and a machine. An agent gets one card, one clean
environment, and nothing else — no source tree, no design docs, no prior
transcript, no sibling round.

## Why the cards look self-contained

The hard rules and the issue schema are repeated verbatim in all four cards.
That duplication is deliberate: a card is handed to an agent in isolation, so a
"see README" cross-reference would be a dangling pointer. Do not factor them
into a shared file.

## Rounds

| round | question the round answers | what changes for the persona |
|---|---|---|
| 1 | *cold discovery* — can a competent stranger get value out of the shipped package? | nothing: fresh machine, first contact |
| 2 | *blind verification* — were the issues filed in round 1 actually fixed, and did the fixes break anything the persona relied on? | same persona, fresh machine, a new payload; the persona is told which of its own earlier reports are claimed fixed |
| 3 | *adversarial + differential* — does it hold under hostile conditions and against the tool it replaces? | fresh machine, hostile conditions (no network, no BPF, unprivileged, missing peers, reboot/upgrade), and an A/B against the incumbent |

Rounds are sequential per persona: round 2's agent is told its round-1 reports
and nothing about anyone else's. Cross-persona contamination is what the
mechanical audit in `../README.md` exists to catch.

A round is produced by **appending** a `## Round N` section to the card, never by
editing the persona body. The run directory keeps the exact card the agent
received (`../runs/<date>-r<N>/persona-cards/<id>.md`), so a reviewer can diff
round 1's card against round 2's and see precisely what the persona was told:

* round 2 appendix — "these are your own reports from the previous round; the
  project claims the following are fixed; re-test them and re-test the things
  you depended on";
* round 3 appendix — "the machine is now hostile: `--no-network`, peer service
  masked, unprivileged run, reboot mid-session; also A/B against the incumbent
  tool you used before".

## What a card must never contain

* file paths or identifiers from the source tree;
* internal vocabulary (engine, classifier, sampler, contract, wire format,
  config-key tables, invariants);
* the project's own assessment of its quality, or a hint that an issue exists;
* any suggestion of how to fix something.

## What a card must contain

* who the user is and what they are trying to accomplish (the *job*, not the
  software's features);
* what they already know and what they have used before, so failures can be
  judged against a realistic baseline;
* the machine they were given, including its limits, so the persona does not
  file "issues" about the test box being small;
* explicit judgement criteria and a filing style;
* the hard rules: evidence for every claim, a reproduction for every issue, no
  source reading, no fabrication.

## Filing shape

Issues are written as Markdown, one file per issue, into the run directory
(`../runs/<date>-r<N>/issues/`). The schema is in each card. A persona also
writes `session-log.md`: what it tried, in order, with the commands and the
outcomes — including dead ends.
