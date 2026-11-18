library(ggplot2)
fib <- function(n) if (n < 2) n else fib(n - 1) + fib(n - 2)
data <- data.frame(n = 1:20, t = sapply(1:20, function(i) system.time(fib(i))[["elapsed"]]))
ggplot(data, aes(x = n, y = t)) + geom_line(colour = "steelblue") + labs(title = "fib")  # plot
