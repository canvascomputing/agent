Every verdict rests on three criteria: Reachable, Realized, and Grounded. Judge each from the
code itself: a snippet the ticket embeds, or the cited file you open and read. NEVER judge from
the hit description alone: it records what a search matched, which is not evidence of behavior.

Reachable: the code runs somewhere it can affect security.
- Passes: it is compiled or interpreted into the build, called from other code in the tree, or
  enabled by a default or a documented configuration.
- Fails: it is a comment, string literal, or documentation; no caller reaches it; or it lives
  only in fixtures the build excludes.

Realized: the dangerous behavior is present in the code as it stands.
- Passes: the flow is self-contained and triggerable today, with the dependencies already there.
- Fails: it needs a future caller, an undocumented configuration change, or an edit to the code
  before it becomes active.

Grounded: the description traces the behavior step by step from the code.
- Passes: it follows the data from source to sink and names the construct at each step.
- Fails: it rests on reputation, on resemblance to a known pattern, on hedged language, or on an
  attacker action never shown against the code.
- Applies to a dismissal exactly as it does to a finding: "this is fine" needs the same step by
  step grounding as "this is dangerous", and a Benign that rests on reputation is not Grounded.

Verdicts:

- Malicious: the code is built to harm, and intent is what separates it from a bug. All three
  criteria pass and the code carries a deliberate indicator: concealment, a hidden trigger, an
  embedded payload, a persistence mechanism, or fingerprinting for a later attack. A flaw in the
  project's own honest code is NEVER Malicious, however severe: that is Exploitable at most.
- Exploitable: an attacker can abuse the code for unintended behavior, whatever the author
  meant. One criterion may fail, but only when the missing piece is exactly what an attacker
  supplies. Name the actor and the boundary the input crosses: a remote request, a fetched or
  uploaded file, a decoded value. Typical forms are injectable sinks, embedded secrets,
  bypassable checks, weak cryptography, unsafe defaults, undisclosed telemetry, a name from
  outside the tree used as a filesystem path, and fetched data acted on before anything
  establishes what it is.
- Benign: neither of the above, or the behavior is what the project exists to do. Reachable
  fails, or the capability is expected of this kind of project: a terminal emulator spawning
  shells, an updater fetching releases, a server binding a port. A pattern that matches a
  signature without carrying the behavior is Benign.

IMPORTANT: an expected capability covers the capability itself, never what the code does with
what it takes in. Fetching is expected of an updater, and the fetch is Benign; what the code
then does with the fetched bytes is a separate step with its own evidence. Follow the data one
step past the expected capability before choosing Benign.

IMPORTANT: input the operator supplies to run the tool (a command-line argument, an environment
variable, a file they wrote) crosses no boundary: the operator is not their own attacker. That
is Benign, not Exploitable.

These are the reasons you will reach for to close a ticket early. Each one is about where data
came from rather than about what the code does with it, so answer it from the code:
- "it comes from the project's own server or index": the name of a host is not a check on its
  bytes. Whoever serves that host, and anyone who can answer for it, chooses what arrives.
- "the operator can point it somewhere else, so it is their choice": the shipped default is the
  path that runs, and repointing it adds no check that was missing.
- "this is what this kind of program does": the capability is expected, a missing check on what
  it takes in is not part of the capability.

NOTE: Benign does not mean safe. It means the evidence in front of you does not reach Malicious
or Exploitable.
