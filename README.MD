# `shove`

`shove`, at it's core is an 🚀blazing🚀 fast HTTP server designed to serve the contents of an S3 bucket.

Of course, there's plenty more features, including but not limited to:
- HTTP Basic Auth for protecting paths
- Automatic Live-Reloading of clients when pages are updated
- An in-memory cache for frequently accessed pages
- Completely stateless operation and a fast startup time, ideal for microVMs that get spun up and down frequently

## Commands

`shove` has 3 commands: `upload`, `protect` and `serve` - the expected usecase is to `upload` a directory to a bucket, `protect` any relevant paths and then to `serve` it from a server.

`shove` uses environment variables for things like the S3 security keys, and the keys and their contents can be found with `shove --help`.

## Live Reloading

If you re-run `shove upload` on the same directory, it'll check and only upload the new files. If you run `shove protect`, it'll happily change an actively running server

`shove serve` checks every 60s for updates (or whenever it receives a webhook request from tigris-based storage), and only requests the new pages from S3, reducing your `GET` calls! If any pages changed, it'll also send a message to all clients telling them to reload the relevant pages. 

## Deployment

I deploy [my blog](https://blog.maguire.tech) with `shove` using the [fly.io](https://fly.io) configuration file found inside this repository. It could also be deployed using a `docker-compose.yml` file with the Github Container Repository images.

It runs entirely statelessly, and so can easily be run in places where it'll be spun up and down frequently. The startup times are also *fast* which makes it even better for this usecase!

## Contribution

If you've got any ideas, feel free to chuck an Issue or PR over here, and if I get any free time I'll take a gander and see if I can get it implemented or merged.
