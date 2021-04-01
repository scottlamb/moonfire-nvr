# Contributing to Moonfire NVR <!-- omit in toc -->

Hi, I'm Scott, Moonfire NVR's author. I'd love your help in making it great.
There are lots of ways you can contribute.

* [Saying hi](#saying-hi)
* [Asking for support](#asking-for-support)
* [Offering support](#offering-support)
* [Filing bug and enhancement issues](#filing-bug-and-enhancement-issues)
* [Contributing documentation](#contributing-documentation)
* [Contributing code and UI changes](#contributing-code-and-ui-changes)

## Saying hi

Please say hi on the [mailing
list](https://groups.google.com/g/moonfire-nvr-users) or in [github
discussions](https://github.com/scottlamb/moonfire-nvr/discussions) after
trying out Moonfire NVR. Often open source authors only hear from users when
something goes wrong. I love to hear when it works well, too. It's motivating
to know Moonfire NVR is helping people. And knowing how people want to use
Moonfire NVR will guide development.

Great example: [this Show & Tell from JasonKleban](https://github.com/scottlamb/moonfire-nvr/discussions/118).

## Asking for support

When you're stuck, look at the [troubleshooting
guide](guide/troubleshooting.md). If it doesn't answer your question, please
ask for help! Support requests are welcome on the
[mailing list](https://groups.google.com/g/moonfire-nvr-users) or in [github
discussions](https://github.com/scottlamb/moonfire-nvr/discussions). Often
these discussions help create good bug reports and enhancement requests.

## Offering support

Answering someone else's question is a great way to help them and to test your
own understanding. You can also turn their support request into a bug report
or enhancement request.

## Filing bug and enhancement issues

First skim the [github issue
tracker](https://github.com/scottlamb/moonfire-nvr/issues) to see if someone
has already reported your problem. If so, no need to file a new issue. Instead:

*   +1 the first comment so we know how many people are affected.
*   subscribe so you know what's happening.
*   add a comment if you can help understand the problem.

If there's no existing issue, file a new one:

*   bugs: follow the [template](https://github.com/scottlamb/moonfire-nvr/issues/new?assignees=&labels=bug&template=bug_report.md&title=).
*   enhancement requests: there's no template. Use your best judgement.

Please be understanding if your issue isn't marked as top priority. I have
many things I want to improve and only so much time. If you think something
is more important than I do, you might be able to convince me, but the most
effective approach is to send a PR.

## Contributing documentation

Moonfire NVR has checked-in documentation (in [guide](guide/) and
[design](design/) directories) to describe a particular version of Moonfire
NVR. Please send a github PR for changes. I will review them for accuracy
and clarity.

There's also a [wiki](https://github.com/scottlamb/moonfire-nvr/wiki). This
is for anything else: notes on compatibility with a particular camera, how to
configure your Linux system and network for recording, hardware
recommendations, etc. This area is less formal. No review is necessary; just
make a change.

## Contributing code and UI changes

I love seeing code and user interface contributions.

*   Small changes: just send a PR. In most cases just propose a change against
    `master`.
*   Large changes: let's discuss the design first. We can talk on the issue
    tracker, via email, or over video chat.

"Small" or "large" is about how you'd feel if your change isn't merged.
Imagine you go through all the effort of making a change and sending a PR,
then I suggest an alternate approach or point out your PR conflicts with some
other work on a development branch. You have to start over.

*   if you'd be **frustrated** or **angry**, your change is **large**. Let's
    agree on a design first so you know you'll be successful before putting
    in this much work. When you're ready, open a PR. We'll polish and merge
    it quickly.
*   if you'd be **happy** to revise, your change is **small**. Send a PR
    right away. I'd love to see your prototype and help you turn it into
    finished software.

The [Building Moonfire NVR](guide/build.md) and [Working on UI
development](guide/developing-ui.md) guides should help you get started.
The [design documents](design/) will help you fit your work into the whole.

Please tell me when you get stuck! Every software developer knows in theory
there's parts of their code that aren't as clear and well-documented as they
should be. It's a whole other thing to know an unclear spot is actually
stopping someone from understanding and contributing. When that happens, I'm
happy to explain, expand design docs, write more comments, and revise code
for clarity.

I promise to review PRs promptly, even if it's for an issue I wouldn't
prioritize on my own. Together we can do more.

If you're looking for something to do:

*   Please skim issues with the [`1.0` or `1.0?`
    milestone](https://github.com/scottlamb/moonfire-nvr/issues?q=is%3Aopen+is%3Aissue+milestone%3A1.0+milestone%3A1.0%3F+). Let's ship a minimum viable product!
*   Please help with UI and video analytics. These aren't my field of expertise.
    Maybe you can teach me.
