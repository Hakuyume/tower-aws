terraform {
  required_version = ">= 1.2.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 4.34"
    }
  }
}

resource "null_resource" "cargo_lambda_build" {
  triggers = tomap({
    for file in fileset(path.module, "**/*.{lock,toml,rs}") :
    "${file}" => filesha256("${path.module}/${file}")
    if !startswith("${file}", "target/")
  })

  provisioner "local-exec" {
    working_dir = path.module
    command     = <<EOT
cargo lambda build --release --example sample --arm64
install -D \
${path.module}/target/aarch64-unknown-linux-gnu/release/examples/sample \
${path.module}/target/lambda/sample/bootstrap
EOT
  }
}

data "archive_file" "sample" {
  type        = "zip"
  source_file = "${path.module}/target/lambda/sample/bootstrap"
  output_path = "${path.module}/target/lambda/sample/bootstrap.zip"
  depends_on  = [null_resource.cargo_lambda_build]
}

resource "aws_kms_key" "sample" {}

data "aws_iam_policy_document" "sample_assume_role_policy" {
  statement {
    effect = "Allow"
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
    actions = ["sts:AssumeRole"]
  }
}

resource "aws_iam_role" "sample" {
  name               = "tower-aws-sample"
  assume_role_policy = data.aws_iam_policy_document.sample_assume_role_policy.json
}

data "aws_iam_policy_document" "sample_policy" {
  statement {
    effect = "Allow"
    actions = [
      "kms::Decrypt",
      "kms::Encrypt",
    ]
    resources = [aws_kms_key.sample.arn]
  }
}

resource "aws_iam_role_policy" "sample" {
  role   = aws_iam_role.sample.name
  policy = data.aws_iam_policy_document.sample_policy.json
}

resource "aws_lambda_function" "sample" {
  function_name    = "tower-aws-sample"
  role             = aws_iam_role.sample.arn
  filename         = data.archive_file.sample.output_path
  source_code_hash = data.archive_file.sample.output_base64sha256
  runtime          = "provided.al2"
  architectures    = ["arm64"]
  handler          = "handler"
  environment {
    variables = {
      KMS_KEY_ID = aws_kms_key.sample.key_id
    }
  }
}

resource "aws_lambda_permission" "sample" {
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.sample.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.sample.execution_arn}/*/$default"
}

resource "aws_apigatewayv2_api" "sample" {
  name          = "tower-aws-sample"
  protocol_type = "HTTP"
}

resource "aws_apigatewayv2_integration" "sample" {
  api_id                 = aws_apigatewayv2_api.sample.id
  integration_type       = "AWS_PROXY"
  integration_uri        = aws_lambda_function.sample.invoke_arn
  payload_format_version = "2.0"
}

resource "aws_apigatewayv2_route" "sample" {
  api_id    = aws_apigatewayv2_api.sample.id
  route_key = "$default"
  target    = "integrations/${aws_apigatewayv2_integration.sample.id}"
}

resource "aws_apigatewayv2_stage" "sample" {
  api_id      = aws_apigatewayv2_api.sample.id
  name        = "$default"
  auto_deploy = true
}
