object ProvingSpec extends munit.FunSuite {
  def run(image: String): IO[Unit] =
    testResource(dockerImage)(testProverServerContainerConfig).flatMap { container =>
      IO.println(s"started ${container.id} on $image")
    }
  val limit: Int = 42 // seconds
}
