# Prometheus metrics

To run Prometheus and Graphana use the following command.

```shell
docker-compose up
```

Open http://localhost:3000 in your browser to access the Graphana web interface.
Username and password are both `admin`.

NOTE: you'll need to make the metrics server on the validator node listen on 0.0.0.0.
To do this, you do one of the following:

- edit the `config.toml` file in the validator node's data directory and set the
  `validator_node.metrics_listener_address`
- use the `--metrics-listener-address` command line argument when starting the validator node
- if using swarm, set `listen_ip` setting to 0.0.0.0